use storing_unicorns::services::query_tabs::{QueryTab, QueryTabsState};

// ========== QueryTab Tests ==========

#[test]
fn query_tab_default() {
    let tab = QueryTab::default();
    assert_eq!(tab.name, "Query 1");
    assert!(tab.query.is_empty());
    assert_eq!(tab.cursor_position, 0);
    assert!(!tab.is_modified);
}

#[test]
fn query_tab_new() {
    let tab = QueryTab::new("My Tab".into());
    assert_eq!(tab.name, "My Tab");
    assert!(tab.query.is_empty());
    assert_eq!(tab.cursor_position, 0);
    assert!(!tab.is_modified);
}

#[test]
fn query_tab_with_query() {
    let tab = QueryTab::with_query("Custom".into(), "SELECT 1".into());
    assert_eq!(tab.name, "Custom");
    assert_eq!(tab.query, "SELECT 1");
    assert_eq!(tab.cursor_position, 8); // length of "SELECT 1"
    assert!(!tab.is_modified);
}

// ========== QueryTabsState Tests ==========

#[test]
fn query_tabs_new() {
    let state = QueryTabsState::new();
    assert_eq!(state.tabs.len(), 1);
    assert_eq!(state.active_tab, 0);
    assert_eq!(state.tabs[0].name, "Query 1");
}

#[test]
fn query_tabs_current_tab() {
    let state = QueryTabsState::new();
    let tab = state.current_tab();
    assert_eq!(tab.name, "Query 1");
}

#[test]
fn query_tabs_current_tab_mut() {
    let mut state = QueryTabsState::new();
    state.current_tab_mut().query = "SELECT * FROM users".into();
    assert_eq!(state.tabs[0].query, "SELECT * FROM users");
}

#[test]
fn query_tabs_add_tab() {
    let mut state = QueryTabsState::new();
    assert_eq!(state.tabs.len(), 1);

    state.add_tab();
    assert_eq!(state.tabs.len(), 2);
    assert_eq!(state.active_tab, 1);
    assert_eq!(state.tabs[1].name, "Query 2");

    state.add_tab();
    assert_eq!(state.tabs.len(), 3);
    assert_eq!(state.active_tab, 2);
    assert_eq!(state.tabs[2].name, "Query 3");
}

#[test]
fn query_tabs_add_tab_with_query() {
    let mut state = QueryTabsState::new();
    state.add_tab_with_query("Analysis".into(), "SELECT count(*) FROM orders".into());
    assert_eq!(state.tabs.len(), 2);
    assert_eq!(state.active_tab, 1);
    assert_eq!(state.tabs[1].name, "Analysis");
    assert_eq!(state.tabs[1].query, "SELECT count(*) FROM orders");
    assert_eq!(state.tabs[1].cursor_position, 27);
}

#[test]
fn query_tabs_close_current_tab_multiple() {
    let mut state = QueryTabsState::new();
    state.add_tab();
    state.add_tab();
    assert_eq!(state.tabs.len(), 3);
    assert_eq!(state.active_tab, 2);

    // Close last tab
    let closed = state.close_current_tab();
    assert!(closed);
    assert_eq!(state.tabs.len(), 2);
    assert_eq!(state.active_tab, 1); // Adjusted to last valid index
}

#[test]
fn query_tabs_close_current_tab_single_not_allowed() {
    let mut state = QueryTabsState::new();
    assert_eq!(state.tabs.len(), 1);

    let closed = state.close_current_tab();
    assert!(!closed); // Can't close the last tab
    assert_eq!(state.tabs.len(), 1);
}

#[test]
fn query_tabs_close_middle_tab() {
    let mut state = QueryTabsState::new();
    state.add_tab(); // Tab 2
    state.add_tab(); // Tab 3

    // Switch to middle tab
    state.switch_to_tab(1);
    assert_eq!(state.active_tab, 1);

    let closed = state.close_current_tab();
    assert!(closed);
    assert_eq!(state.tabs.len(), 2);
    // active_tab stays at 1 (now pointing to what was tab 3)
    assert_eq!(state.active_tab, 1);
}

#[test]
fn query_tabs_next_tab() {
    let mut state = QueryTabsState::new();
    state.add_tab();
    state.add_tab();
    state.switch_to_tab(0);

    state.next_tab();
    assert_eq!(state.active_tab, 1);

    state.next_tab();
    assert_eq!(state.active_tab, 2);

    // Wrap around
    state.next_tab();
    assert_eq!(state.active_tab, 0);
}

#[test]
fn query_tabs_prev_tab() {
    let mut state = QueryTabsState::new();
    state.add_tab();
    state.add_tab();
    state.switch_to_tab(0);

    // Prev from 0 wraps to last
    state.prev_tab();
    assert_eq!(state.active_tab, 2);

    state.prev_tab();
    assert_eq!(state.active_tab, 1);
}

#[test]
fn query_tabs_switch_to_tab() {
    let mut state = QueryTabsState::new();
    state.add_tab();
    state.add_tab();

    state.switch_to_tab(0);
    assert_eq!(state.active_tab, 0);

    state.switch_to_tab(2);
    assert_eq!(state.active_tab, 2);

    // Invalid index => no change
    state.switch_to_tab(99);
    assert_eq!(state.active_tab, 2);
}

#[test]
fn query_tabs_rename_current_tab() {
    let mut state = QueryTabsState::new();
    state.rename_current_tab("My Custom Query".into());
    assert_eq!(state.tabs[0].name, "My Custom Query");
}

#[test]
fn query_tabs_next_tab_single() {
    let mut state = QueryTabsState::new();
    state.next_tab();
    assert_eq!(state.active_tab, 0); // Only one tab, stays
}

#[test]
fn query_tabs_prev_tab_single() {
    let mut state = QueryTabsState::new();
    state.prev_tab();
    assert_eq!(state.active_tab, 0); // Only one tab, stays
}
