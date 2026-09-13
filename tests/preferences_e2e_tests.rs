// SPDX-License-Identifier: GPL-3.0-or-later
//
// Evolution GTK4 Preferences & Settings Complete 4-Tier E2E Test Suite
//
// Tier 1: Feature Coverage (>=5 tests per feature for each preference category)
// Tier 2: Boundary & Corner Cases (>=5 tests per category where boundaries exist)
// Tier 3: Cross-Feature Interactions & Combinations (pairwise coverage)
// Tier 4: Real-World Application Workloads (persistence, reset, backup/restore, signal dispatch)

mod suites;

#[test]
fn e2e_suite_initialization() {
    let schema = suites::common::get_test_schema();
    assert_eq!(schema.id(), "org.gnome.Evolution4");
    assert!(schema.has_key("active-view"));
    assert!(schema.has_key("color-scheme"));
    assert!(schema.has_key("mail-layout"));
}
