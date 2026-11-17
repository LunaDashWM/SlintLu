//! The `I18n` global and the gettext catalogs compiled from the shell's `.po` files.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use slint_interpreter::Value;
use slintlu::i18n;

use crate::scripting::ShellHost;
use crate::scripting::bridge::{list_value, string, string_arg};

/// The gettext domain shells are compiled with.
pub const TRANSLATION_DOMAIN: &str = "slintlu";

/// Compiles `<config dir>/i18n/*.po` and makes gettext use them. Called before every load so
/// edited translations apply on reload.
pub fn load_translations(host: &ShellHost) {
    let Some(source) = host.config().parent().map(|d| d.join("i18n")).filter(|d| d.is_dir()) else {
        host.set_global("I18n", "languages", list_value([]));
        return;
    };
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    let target = crate::config::data_home().join("i18n").join(format!("{:016x}", hasher.finish()));
    match i18n::compile_catalogs(&source, &target, TRANSLATION_DOMAIN) {
        Ok(compiled) => {
            slint::private_unstable_api::init_translations(TRANSLATION_DOMAIN, compiled.locale_dir);
            log::info!("translations: {}", compiled.languages.join(", "));
            host.set_global("I18n", "languages", list_value(compiled.languages.iter().map(string)));
            slint::update_all_translations();
        }
        Err(e) => log::warn!("cannot compile translations in {}: {e}", source.display()),
    }
}

pub fn install(host: &Rc<ShellHost>) {
    host.set_global("I18n", "language", string(i18n::current_language().unwrap_or_default()));
    host.on_callback(
        "I18n",
        "language-name",
        Rc::new(|args| string(i18n::language_name(&string_arg(args, 0).unwrap_or_default()))),
    );
    let weak = Rc::downgrade(host);
    host.on_callback(
        "I18n",
        "set-language",
        Rc::new(move |args| {
            let language = string_arg(args, 0).unwrap_or_default();
            // An empty LANGUAGE makes gettext fall back to LC_MESSAGES/LANG, so "" (the
            // untranslated texts) would still show the system language. "C" stops the lookup.
            let gettext_language = if language.is_empty() { "C" } else { language.as_str() };
            // SAFETY: gettext reads LANGUAGE on every lookup and offers no other way to switch
            // languages at runtime. SlintLu's own threads never read the environment after
            // startup, so nothing races with this write.
            unsafe { std::env::set_var("LANGUAGE", gettext_language) };
            slint::update_all_translations();
            if let Some(host) = weak.upgrade() {
                host.set_global("I18n", "language", string(&language));
            }
            Value::Void
        }),
    );
}
