// SPDX-License-Identifier: GPL-3.0-or-later
//
// Evolution GTK4 Advanced Features (R1 through R6) End-to-End Test Suite
//
// Covers:
// - R1: Standalone Message Viewer Window (activation, actions, multi-window, lifecycle)
// - R2: Desktop Drag-and-Drop (Composer file drop, Mail list drag/drop to sidebar folders, Calendar time grid drag)
// - R3: Account Autodiscover & Wizard Modernization (Mozilla ISPDB, DNS MX, known provider profiles)
// - R4: Native macOS Keychain Integration (Security.framework bindings under com.martingawron.Meridian, Linux compilation cleanliness)
// - R5: Standalone Portable IMAP/SMTP Engine for macOS (offline/mock-server tests for IMAP listing, envelope/body parse, direct SMTP delivery)
// - R6: Interactive Certificate & Security Manager (X.509 parsing, OpenPGP, import/export, trust levels)
//
// Organized across:
// - Tier 1: Direct Feature Coverage (R1 to R6 baseline behavior)
// - Tier 2: Boundary & Corner Cases (limits, intervals, unicode, special characters, edge slots)
// - Tier 3: Pairwise Cross-Feature Interactions
// - Tier 4: Real-World Application Workloads & Onboarding Flows
// - Tier 5: Adversarial Hardening & Boundary Stress

mod advanced_features_suites;
#[path = "advanced_features_suites/r4_r5_macos.rs"]
mod r4_r5_macos;

#[test]
fn advanced_features_e2e_suite_initialization() {
    let schema = advanced_features_suites::common::get_test_schema();
    assert_eq!(schema.id(), "com.martingawron.Meridian");
    assert!(schema.has_key("mail-image-loading-policy"));
    assert!(schema.has_key("smime-signer-certificate"));
    assert!(schema.has_key("smime-ca-file"));
    assert!(schema.has_key("offline-mode"));
}
