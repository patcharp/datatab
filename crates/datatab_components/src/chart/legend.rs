//! The legend element for charts.
//!
//! The element moved to `bolt::chart::legend`; re-exported at this path so
//! call sites keep compiling. Its tooltip hint is asked by key from the
//! installed translator (`bolt::strings`), which this crate's theme adapter
//! answers from `datatab_i18n`; the tests here pin that the catalogue answers
//! the two keys the legend asks for.

pub use bolt::chart::legend::legend_element;

#[cfg(test)]
mod tests {
    #[test]
    fn legend_keys_resolve() {
        let en_hide = datatab_i18n::t!("chart.legend.hide_series", locale = "en");
        let en_show = datatab_i18n::t!("chart.legend.show_series", locale = "en");

        assert!(!en_hide.is_empty() && !en_hide.starts_with("en."));
        assert!(!en_show.is_empty() && !en_show.starts_with("en."));
    }
}
