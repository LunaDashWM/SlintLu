//! [`ShellHost`]: owns the compiled shell and its window instances.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::ComponentHandle;
use slint_interpreter::{ComponentDefinition, ComponentInstance, Value};
use slintlu::{OutputInfo, SurfaceRole};

use super::bridge::{self, Bridge, Handler};
use super::compiler;
use super::panel::{self, PanelSpec, WindowKind};

/// One live window of the shell.
struct Entry {
    component: String,
    /// The output a panel belongs to; `None` for floating windows.
    output: Option<u32>,
    instance: ComponentInstance,
}

impl Drop for Entry {
    fn drop(&mut self) {
        // A shown window keeps its component alive; hide it so it is really released.
        let _ = self.instance.hide();
    }
}

pub struct ShellHost {
    config: PathBuf,
    definitions: RefCell<Vec<ComponentDefinition>>,
    entries: RefCell<Vec<Entry>>,
    /// Components inheriting `LockWindow`; the first one is used as the lock screen.
    lock_definitions: RefCell<Vec<ComponentDefinition>>,
    /// Lock screens, one per output, while the session is locked.
    lock_entries: RefCell<Vec<Entry>>,
    bridge: Bridge,
}

thread_local! {
    static HOST: RefCell<Option<Rc<ShellHost>>> = const { RefCell::new(None) };
}

/// Runs `f` with the host of this thread, if one is running.
pub fn with<R>(f: impl FnOnce(&Rc<ShellHost>) -> R) -> Option<R> {
    let host = HOST.with(|h| h.borrow().clone())?;
    Some(f(&host))
}

/// Drops all windows and the host while the rest of the thread's state (renderer caches) is
/// still alive. Call after the event loop has returned.
pub fn shutdown() {
    super::globals::shutdown();
    if let Some(host) = HOST.with(|h| h.borrow_mut().take()) {
        host.entries.borrow_mut().clear();
        host.lock_entries.borrow_mut().clear();
    }
}

/// Runs `f` on the Slint event loop with the host. Usable from any thread.
pub fn post(f: impl FnOnce(&Rc<ShellHost>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        with(f);
    });
}

impl ShellHost {
    /// Creates the host and makes it reachable through [`with`] and [`post`].
    pub fn install(config: PathBuf) -> Rc<Self> {
        let host = Rc::new(Self {
            config,
            definitions: Default::default(),
            entries: Default::default(),
            lock_definitions: Default::default(),
            lock_entries: Default::default(),
            bridge: Bridge::default(),
        });
        HOST.with(|h| *h.borrow_mut() = Some(host.clone()));
        host
    }

    /// The shell's entry file.
    pub fn config(&self) -> &std::path::Path {
        &self.config
    }

    /// Compiles the configuration and replaces all windows. On compile errors the running shell
    /// is kept and `false` is returned.
    pub fn load(&self) -> bool {
        super::globals::load_translations(self);
        super::globals::load_theme(self);
        let compiled = match compiler::compile(&self.config) {
            Ok(compiled) => compiled,
            Err(diagnostics) => {
                slint_interpreter::print_diagnostics(&diagnostics);
                log::error!("{} has errors, keeping the current shell", self.config.display());
                return false;
            }
        };
        let definitions: Vec<_> = compiled.result.components().collect();
        log::info!("loaded {} ({} exported components)", self.config.display(), definitions.len());
        // Drop the old windows before creating new ones, so namespaces and exclusive zones
        // never overlap.
        self.entries.borrow_mut().clear();
        self.lock_definitions.borrow_mut().clear();
        *self.definitions.borrow_mut() = definitions;
        self.instantiate();
        super::globals::refresh(self);
        true
    }

    /// Publishes a global property value to all windows, now and after reloads.
    pub fn set_global(&self, global: &str, property: &str, value: Value) {
        self.each_instance(|instance| {
            bridge::apply_property(instance, global, property, value.clone())
        });
        self.bridge.set_property(global, property, value);
    }

    /// Implements a global callback natively in all windows, now and after reloads.
    pub fn on_callback(&self, global: &str, callback: &str, handler: Handler) {
        self.each_instance(|instance| {
            bridge::apply_callback(instance, global, callback, handler.clone())
        });
        self.bridge.set_callback(global, callback, handler);
    }

    /// Reads a global property from the first window that has it.
    pub fn global_property(&self, global: &str, property: &str) -> Option<Value> {
        self.entries
            .borrow()
            .iter()
            .find_map(|e| e.instance.get_global_property(global, property).ok())
    }

    /// Sets a global property in every window (for globals the shell itself owns, which are
    /// per window). Fails if no window has it.
    pub fn set_global_everywhere(
        &self,
        global: &str,
        property: &str,
        value: Value,
    ) -> Result<(), String> {
        let mut last_error = format!("no window exports the global {global}");
        let mut any = false;
        self.each_instance(|instance| {
            match instance.set_global_property(global, property, value.clone()) {
                Ok(()) => any = true,
                Err(e) => last_error = format!("cannot set {global}.{property}: {e}"),
            }
        });
        if any { Ok(()) } else { Err(last_error) }
    }

    /// Invokes a global callback or function. Natively implemented callbacks run once; the
    /// shell's own ones run in every window (their state is per window). Returns the first
    /// result.
    pub fn invoke_global(&self, global: &str, name: &str, args: &[Value]) -> Option<Value> {
        if let Some(handler) = self.bridge.callback(global, name) {
            return Some(handler(args));
        }
        let mut result = None;
        self.each_instance(|instance| match instance.invoke_global(global, name, args) {
            Ok(value) => {
                result.get_or_insert(value);
            }
            Err(e) => log::debug!("cannot invoke {global}.{name}: {e}"),
        });
        result
    }

    /// The components currently shown, with the id of their output.
    pub fn windows(&self) -> Vec<(String, Option<u32>)> {
        self.entries.borrow().iter().map(|e| (e.component.clone(), e.output)).collect()
    }

    /// Runs `f` for every window instance, e.g. for per-instance values like formatted time.
    pub fn each_instance(&self, f: impl FnMut(&ComponentInstance)) {
        let instances: Vec<_> = self
            .entries
            .borrow()
            .iter()
            .chain(self.lock_entries.borrow().iter())
            .map(|e| e.instance.clone_strong())
            .collect();
        instances.iter().for_each(f);
    }

    /// Adds panels for new outputs, removes those of vanished ones and refreshes screen info.
    pub fn outputs_changed(&self) {
        let outputs = slintlu::outputs();
        self.entries
            .borrow_mut()
            .retain(|e| e.output.is_none_or(|id| outputs.iter().any(|o| o.id == id)));
        self.set_global("Shell", "screens", bridge::screens_value(&outputs));
        for entry in self.entries.borrow().iter() {
            if let Some(output) = outputs.iter().find(|o| Some(o.id) == entry.output) {
                bridge::apply_property(
                    &entry.instance,
                    "Shell",
                    "screen",
                    bridge::screen_value(output),
                );
            }
        }
        self.lock_entries
            .borrow_mut()
            .retain(|e| e.output.is_none_or(|id| outputs.iter().any(|o| o.id == id)));
        if slintlu::security::session_locked() {
            self.show_lock_screens();
        }
        self.instantiate();
        super::globals::refresh(self);
    }

    /// Whether the shell defines a lock screen.
    pub fn has_lock_screen(&self) -> bool {
        !self.lock_definitions.borrow().is_empty()
    }

    /// Covers every output without one with a lock screen. Call after
    /// [`slintlu::security::lock_session`].
    pub fn show_lock_screens(&self) {
        let Some(definition) = self.lock_definitions.borrow().first().cloned() else { return };
        for output in slintlu::outputs() {
            if self.lock_entries.borrow().iter().any(|e| e.output == Some(output.id)) {
                continue;
            }
            let Some(instance) = self.create(&definition, Some(&output)) else { continue };
            if let Some(window) = slintlu::window_for(instance.window()) {
                window.set_role(SurfaceRole::Lock { output: output.id });
            }
            if let Err(e) = instance.show() {
                log::error!("cannot show the lock screen: {e}");
            }
            self.lock_entries.borrow_mut().push(Entry {
                component: definition.name().to_string(),
                output: Some(output.id),
                instance,
            });
        }
    }

    /// Destroys the lock screens. Call before [`slintlu::security::unlock_session`].
    pub fn hide_lock_screens(&self) {
        self.lock_entries.borrow_mut().clear();
    }

    /// Creates every missing window.
    fn instantiate(&self) {
        let outputs = slintlu::outputs();
        self.set_global("Shell", "screens", bridge::screens_value(&outputs));
        let definitions = self.definitions.borrow().clone();
        for definition in &definitions {
            self.instantiate_component(definition, &outputs);
        }
    }

    fn instantiate_component(&self, definition: &ComponentDefinition, outputs: &[OutputInfo]) {
        let name = definition.name().to_string();
        let has = |output: Option<u32>| {
            self.entries.borrow().iter().any(|e| e.component == name && e.output == output)
        };
        if has(None) {
            return;
        }

        // Panels: one instance per matching output.
        for output in outputs {
            if has(Some(output.id)) {
                continue;
            }
            let Some(instance) = self.create(definition, Some(output)) else { return };
            match panel::window_kind(&instance) {
                WindowKind::Panel(spec) => {
                    if spec.shows_on(&output.name) {
                        self.show_panel(&name, instance, spec, output.id);
                    }
                }
                WindowKind::Floating { app_id } => {
                    self.show_floating(&name, instance, app_id);
                    return;
                }
                WindowKind::Lock => {
                    self.lock_definitions.borrow_mut().push(definition.clone());
                    return;
                }
                WindowKind::Other => {
                    log::debug!("{name} is not a PanelWindow or FloatingWindow, skipping it");
                    return;
                }
            }
        }

        // No outputs yet: floating windows can still be shown.
        if outputs.is_empty()
            && let Some(instance) = self.create(definition, None)
            && let WindowKind::Floating { app_id } = panel::window_kind(&instance)
        {
            self.show_floating(&name, instance, app_id);
        }
    }

    fn create(
        &self,
        definition: &ComponentDefinition,
        output: Option<&OutputInfo>,
    ) -> Option<ComponentInstance> {
        let instance = definition
            .create()
            .inspect_err(|e| log::error!("cannot create {}: {e}", definition.name()))
            .ok()?;
        self.bridge.apply(&instance);
        if let Some(output) = output {
            bridge::apply_property(&instance, "Shell", "screen", bridge::screen_value(output));
        }
        // Triggers the windows' `ready` callback once the instance is registered and shown.
        let weak = instance.as_weak();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(instance) = weak.upgrade() {
                bridge::apply_property(&instance, "SlintLuRuntime", "ready", Value::Bool(true));
            }
        });
        Some(instance)
    }

    fn show_panel(&self, name: &str, instance: ComponentInstance, spec: PanelSpec, output: u32) {
        let Some(window) = slintlu::window_for(instance.window()) else { return };
        let role = move |spec: &PanelSpec| {
            SurfaceRole::Layer(slintlu::LayerConfig { output: Some(output), ..spec.layer.clone() })
        };
        window.set_role(role(&spec));
        bridge::apply_property(
            &instance,
            "Shell",
            "renderer",
            bridge::string(window.renderer_kind().as_str()),
        );

        // Follow later changes of the panel's properties (anchors, layer, `shown` …).
        let weak = instance.as_weak();
        let _ = instance.set_global_callback("SlintLuRuntime", "panel-changed", move |_| {
            if let Some(instance) = weak.upgrade()
                && let Some(window) = slintlu::window_for(instance.window())
                && let WindowKind::Panel(spec) = panel::window_kind(&instance)
            {
                // Unmap before touching the layer state: a configure answering a commit made
                // right before unmapping would arrive stale and be a protocol error.
                let result = if spec.shown {
                    window.set_role(role(&spec));
                    instance.show()
                } else {
                    let hidden = instance.hide();
                    window.set_role(role(&spec));
                    hidden
                };
                if let Err(e) = result {
                    log::error!("cannot change panel visibility: {e}");
                }
            }
            Value::Void
        });

        if spec.shown
            && let Err(e) = instance.show()
        {
            log::error!("cannot show {name}: {e}");
        }
        self.entries.borrow_mut().push(Entry {
            component: name.to_string(),
            output: Some(output),
            instance,
        });
    }

    fn show_floating(&self, name: &str, instance: ComponentInstance, app_id: String) {
        if let Some(window) = slintlu::window_for(instance.window()) {
            window.set_role(SurfaceRole::Toplevel { app_id });
            bridge::apply_property(
                &instance,
                "Shell",
                "renderer",
                bridge::string(window.renderer_kind().as_str()),
            );
        }
        if let Err(e) = instance.show() {
            log::error!("cannot show {name}: {e}");
        }
        self.entries.borrow_mut().push(Entry {
            component: name.to_string(),
            output: None,
            instance,
        });
    }
}
