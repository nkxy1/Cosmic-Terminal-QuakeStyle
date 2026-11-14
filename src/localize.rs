// SPDX-License-Identifier: GPL-3.0-only
//! Translations (English and German). The language is picked from the user's locale
//! (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`); anything else falls back to English.
//!
//! Derived from cosmic-term's `localize.rs`.

use std::sync::LazyLock;

use i18n_embed::{
    DefaultLocalizer, LanguageLoader, Localizer,
    fluent::{FluentLanguageLoader, fluent_language_loader},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

pub static LANGUAGE_LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| {
    let loader: FluentLanguageLoader = fluent_language_loader!();
    // No invisible bidi marks around arguments: the text also ends up in a terminal.
    loader.set_use_isolating(false);
    loader
        .load_fallback_language(&Localizations)
        .expect("Error while loading fallback language");
    loader
});

/// Look up a message; arguments are passed as `name = value`.
#[macro_export]
macro_rules! fl {
    ($message_id:literal) => {{
        i18n_embed_fl::fl!($crate::localize::LANGUAGE_LOADER, $message_id)
    }};

    ($message_id:literal, $($args:expr),*) => {{
        i18n_embed_fl::fl!($crate::localize::LANGUAGE_LOADER, $message_id, $($args), *)
    }};
}

/// Select the language that matches the desktop's locale. Call once at startup.
pub fn localize() {
    let localizer: Box<dyn Localizer> =
        Box::new(DefaultLocalizer::new(&*LANGUAGE_LOADER, &Localizations));
    let requested = i18n_embed::DesktopLanguageRequester::requested_languages();
    if let Err(error) = localizer.select(&requested) {
        eprintln!("Error while loading language: {error}");
    }
    // Selecting a language rebuilds the bundles, so the setting has to be repeated.
    LANGUAGE_LOADER.set_use_isolating(false);
}
