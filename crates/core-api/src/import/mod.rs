//! Importers that turn external configuration into vault entities.

mod ssh_config;

pub use ssh_config::{
    ImportPlan, ImportReport, KeyFailure, plan_ssh_config_import, plan_ssh_config_import_with_home,
};
