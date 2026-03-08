//! Light-E2E grouped suite for set-operation and sublink coverage.

#![allow(clippy::duplicate_mod)]

#[path = "e2e_full_join_tests.rs"]
mod e2e_full_join_tests;
#[path = "e2e_set_operation_tests.rs"]
mod e2e_set_operation_tests;
#[path = "e2e_sublink_or_tests.rs"]
mod e2e_sublink_or_tests;
