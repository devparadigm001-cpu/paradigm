//! Map Terminator's raw role strings onto the fixed `control_role` enum locked
//! in migration 20260803000001.
//!
//! ## Why this mapping is needed at all
//!
//! Terminator's `UIElement::role()` is `ControlType::to_string()`, so it can
//! return any of the ~41 Windows UI Automation control types. Our schema locks
//! seven values. Everything that does not map lands on `other` rather than
//! erroring -- an unrecognised control is a step we can still record and
//! replay, just with less semantic detail.
//!
//! ## Casing is not consistent upstream
//!
//! `terminator-workflow-recorder` lowercases the role on click events
//! (`element.role().to_lowercase()`), while text-input events carry
//! `field_type` with its original casing. Step 4b's captures showed both in one
//! session: `role="button"` on clicks and `role="Edit"` on typing. Matching is
//! therefore case-insensitive.

/// The fixed `control_role` enum from migration 20260803000001.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRole {
    Button,
    Textbox,
    Dropdown,
    Checkbox,
    Radio,
    Link,
    Other,
}

impl ControlRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ControlRole::Button => "button",
            ControlRole::Textbox => "textbox",
            ControlRole::Dropdown => "dropdown",
            ControlRole::Checkbox => "checkbox",
            ControlRole::Radio => "radio",
            ControlRole::Link => "link",
            ControlRole::Other => "other",
        }
    }
}

/// Map a raw Terminator role to the locked enum.
///
/// Judgement calls worth stating, since the target enum is coarser than UIA:
///   * `MenuItem`, `TabItem`, `SplitButton` -> `button`. They are activatable
///     controls; the schema has no finer category and "you click it to do
///     something" is the property that matters for replay.
///   * `Document` -> `textbox`. Notepad's editor surfaces as `Document` and is
///     genuinely typed into.
///   * `Text` -> `other`, NOT `textbox`. In UIA `Text` is a static label. Step
///     4b captured a click on `role="text"` that was a caption, not a field.
///   * `List` / `ListItem` -> `other`, not `dropdown`. A listbox is not a
///     dropdown, and conflating them would mislead replay.
pub fn map_role(raw: &str) -> ControlRole {
    match raw.trim().to_ascii_lowercase().as_str() {
        "button" | "splitbutton" | "menuitem" | "tabitem" => ControlRole::Button,
        "edit" | "document" => ControlRole::Textbox,
        "combobox" => ControlRole::Dropdown,
        "checkbox" => ControlRole::Checkbox,
        "radiobutton" => ControlRole::Radio,
        "hyperlink" => ControlRole::Link,
        _ => ControlRole::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_locked_seven() {
        assert_eq!(map_role("Button"), ControlRole::Button);
        assert_eq!(map_role("Edit"), ControlRole::Textbox);
        assert_eq!(map_role("ComboBox"), ControlRole::Dropdown);
        assert_eq!(map_role("CheckBox"), ControlRole::Checkbox);
        assert_eq!(map_role("RadioButton"), ControlRole::Radio);
        assert_eq!(map_role("Hyperlink"), ControlRole::Link);
        assert_eq!(map_role("Slider"), ControlRole::Other);
    }

    #[test]
    fn matching_is_case_insensitive() {
        // Both casings occur in one captured session: clicks are lowercased
        // upstream, typing events are not.
        assert_eq!(map_role("button"), ControlRole::Button);
        assert_eq!(map_role("BUTTON"), ControlRole::Button);
        assert_eq!(map_role("document"), ControlRole::Textbox);
        assert_eq!(map_role("Document"), ControlRole::Textbox);
    }

    #[test]
    fn activatable_controls_become_button() {
        assert_eq!(map_role("MenuItem"), ControlRole::Button);
        assert_eq!(map_role("TabItem"), ControlRole::Button);
        assert_eq!(map_role("SplitButton"), ControlRole::Button);
    }

    #[test]
    fn static_text_is_not_a_textbox() {
        assert_eq!(map_role("Text"), ControlRole::Other);
    }

    #[test]
    fn unknown_roles_fall_back_rather_than_error() {
        assert_eq!(map_role("SemanticZoom"), ControlRole::Other);
        assert_eq!(map_role("unknown"), ControlRole::Other);
        assert_eq!(map_role(""), ControlRole::Other);
    }
}
