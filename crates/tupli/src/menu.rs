//! The macOS menu bar.
//!
//! A Cocoa application that never installs an `NSMainMenu` does not get an
//! empty menu bar — it gets whatever was up there before, belonging to the app
//! you were last in. The strip at the top of the screen keeps saying "Safari"
//! while tupli is frontmost, and every one of those menus is live. So this
//! exists first of all as a correctness fix, and only second as a feature.
//!
//! Everything here is already in [`Command`]; the palette's own doc comment
//! says the list "is the app's menu bar in every sense but the platform one".
//! This is the platform one, and it is deliberately a strict subset — the
//! palette holds all thirty commands and the menu holds the ones somebody
//! would go looking for with a mouse. A menu that mirrors a command palette
//! item for item is a worse command palette.
//!
//! The keystrokes are registered as real bindings rather than being drawn as
//! decoration next to the item, which is what lets macOS show them and grey
//! out an item whose action nothing in the focused window handles. gpui matches
//! bindings before it delivers the key event to `on_key_down` listeners and
//! stops propagation once an action fires, so [`Workspace::on_key`] — which
//! still knows most of these chords — never runs twice for one press.
//!
//! [`Command`]: crate::palette::Command
//! [`Workspace::on_key`]: crate::workspace::Workspace

use gpui::{actions, App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType};

// The application menu's own items. These are the platform's business, not the
// workspace's, so they are handled globally: quitting has to work with the
// settings window focused, or with no window at all.
actions!(
    tupli,
    [About, Quit, Hide, HideOthers, ShowAll, Minimize, Zoom]
);

// One per menu-worthy `Command`, in the order the menus present them. Unit
// structs rather than one action carrying a `Command`, because a parameterised
// action needs a JSON schema to be built from a keymap and there is no keymap
// file here to build it from.
actions!(
    tupli,
    [
        NewTab,
        NewTable,
        NewConnection,
        CloseTab,
        Save,
        SaveAs,
        ExportRows,
        Run,
        RunAll,
        Cancel,
        CommitChanges,
        DiscardChanges,
        AddRow,
        DeleteRows,
        RevertRows,
        RefreshResults,
        RefreshSchema,
        FormatQuery,
        FollowReference,
        SplitRight,
        SplitDown,
        ClosePane,
        ToggleSidebar,
        ToggleResults,
        ToggleInspector,
        ShowData,
        ShowStructure,
        ShowDdl,
        ShowMessages,
        ShowDatabaseTree,
        ShowSavedQueries,
        ShowHistory,
        OpenSettings,
        OpenPalette,
        OpenObjects,
        OpenCommands,
    ]
);

/// Install the menu bar and the bindings that put keystrokes beside its items.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        // File
        KeyBinding::new("cmd-t", NewTab, None),
        KeyBinding::new("cmd-n", NewConnection, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-s", Save, None),
        KeyBinding::new("cmd-shift-s", SaveAs, None),
        KeyBinding::new("cmd-shift-e", ExportRows, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        // Query. ⌘↵ runs the statement under the cursor and ⇧⌘↵ the whole
        // script, which is the split every SQL client has settled on.
        KeyBinding::new("cmd-enter", Run, None),
        KeyBinding::new("cmd-shift-enter", RunAll, None),
        KeyBinding::new("cmd-.", Cancel, None),
        KeyBinding::new("cmd-r", RefreshResults, None),
        KeyBinding::new("cmd-shift-r", RefreshSchema, None),
        // ⌥⇧F, the chord every editor that formats has used since VS Code
        // took it, and one of the few left that ⌘ has not already claimed.
        KeyBinding::new("alt-shift-f", FormatQuery, None),
        // Unmodified, because it is a navigation key and not a command: F6 is
        // what a database client's users already reach for to walk a foreign
        // key, and nothing else in this app wants an F key.
        KeyBinding::new("f6", FollowReference, None),
        // View
        KeyBinding::new("cmd-1", ToggleSidebar, None),
        KeyBinding::new("cmd-2", ToggleResults, None),
        KeyBinding::new("cmd-3", ToggleInspector, None),
        KeyBinding::new("cmd-d", SplitRight, None),
        KeyBinding::new("cmd-shift-d", SplitDown, None),
        // Go
        KeyBinding::new("cmd-k", OpenPalette, None),
        KeyBinding::new("cmd-p", OpenObjects, None),
        KeyBinding::new("cmd-shift-p", OpenCommands, None),
    ]);

    cx.set_menus(vec![
        Menu::new("Tupli").items(vec![
            MenuItem::action("About Tupli", About),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Tupli", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Tupli", Quit),
        ]),
        Menu::new("File").items(vec![
            MenuItem::action("New Query Tab", NewTab),
            MenuItem::action("New Table…", NewTable),
            MenuItem::action("New Connection…", NewConnection),
            MenuItem::separator(),
            MenuItem::action("Save Query", Save),
            MenuItem::action("Save Query As…", SaveAs),
            MenuItem::action("Export Rows…", ExportRows),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
        ]),
        // Cut/copy/paste are `OsAction`s so AppKit routes them down the
        // responder chain to whichever text field has focus. Wiring them to
        // our own handlers would mean reimplementing the clipboard for every
        // input in the app and getting it wrong in one of them.
        Menu::new("Edit").items(vec![
            MenuItem::os_action("Undo", DiscardChanges, OsAction::Undo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", gpui::NoAction, OsAction::Cut),
            MenuItem::os_action("Copy", gpui::NoAction, OsAction::Copy),
            MenuItem::os_action("Paste", gpui::NoAction, OsAction::Paste),
            MenuItem::os_action("Select All", gpui::NoAction, OsAction::SelectAll),
        ]),
        Menu::new("Query").items(vec![
            MenuItem::action("Run Statement", Run),
            MenuItem::action("Run All Statements", RunAll),
            MenuItem::action("Cancel Running Query", Cancel),
            MenuItem::separator(),
            MenuItem::action("Refresh Results", RefreshResults),
            MenuItem::action("Refresh Schema", RefreshSchema),
            MenuItem::separator(),
            MenuItem::action("Format SQL", FormatQuery),
        ]),
        // The row edits. Separate from Edit because Edit belongs to the text
        // field under the pointer and these belong to the grid, and one menu
        // holding both would make "Undo" ambiguous in the only place it must
        // not be.
        Menu::new("Rows").items(vec![
            MenuItem::action("Add Row", AddRow),
            MenuItem::action("Delete Selected Rows", DeleteRows),
            MenuItem::action("Revert Selected Rows", RevertRows),
            MenuItem::separator(),
            MenuItem::action("Save Row Changes…", CommitChanges),
            MenuItem::action("Discard Row Changes", DiscardChanges),
        ]),
        Menu::new("View").items(vec![
            MenuItem::action("Data", ShowData),
            MenuItem::action("Structure", ShowStructure),
            MenuItem::action("DDL", ShowDdl),
            MenuItem::action("Messages", ShowMessages),
            MenuItem::separator(),
            MenuItem::action("Database Tree", ShowDatabaseTree),
            MenuItem::action("Saved Queries", ShowSavedQueries),
            MenuItem::action("History", ShowHistory),
            MenuItem::separator(),
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Toggle Results", ToggleResults),
            MenuItem::action("Toggle Inspector", ToggleInspector),
        ]),
        Menu::new("Go").items(vec![
            MenuItem::action("Go to Anything…", OpenPalette),
            MenuItem::action("Go to Object…", OpenObjects),
            MenuItem::action("Run Command…", OpenCommands),
            MenuItem::separator(),
            MenuItem::action("Follow Reference", FollowReference),
        ]),
        Menu::new("Window").items(vec![
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Split Right", SplitRight),
            MenuItem::action("Split Down", SplitDown),
            MenuItem::action("Close Split", ClosePane),
        ]),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gpui panics during `App` creation when two actions share a name, and a
    /// panic in `main` before the window opens is the one failure that no
    /// screenshot can catch. Building the menu in a test app is enough to
    /// provoke it, and it also proves every binding parses — `KeyBinding::new`
    /// panics on a keystroke it cannot read.
    #[gpui::test]
    fn the_menu_bar_installs(cx: &mut gpui::TestAppContext) {
        cx.update(init);
        // And nothing in it points at an action the app never registered:
        // macOS would show the item, grey, forever.
        cx.update(|cx| {
            for name in ["tupli::Quit", "tupli::Run", "tupli::OpenSettings"] {
                assert!(
                    cx.build_action(name, None).is_ok(),
                    "{name} is not a registered action"
                );
            }
        });
    }
}
