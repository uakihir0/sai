#![forbid(unsafe_code)]

pub mod git_remote;
pub mod git_source_index;
pub mod git_tags;
pub mod git_tree;
pub mod identity;
pub mod lexer;
pub mod local_source;
pub mod lock;
pub mod lock_graph_validation;
pub mod lock_root_contract;
pub mod lock_serialization;
pub mod lock_validation;
pub mod manifest;
pub mod mvs;
pub mod mvs_diagnostics;
pub mod mvs_explanations;
pub mod mvs_graph_validation;
pub mod package_cache;
pub mod package_path;
pub mod resolver_contract;
pub mod resolver_source;
pub mod resolver_source_provider;
pub mod tree_hash;
mod unicode_data_16;
pub mod validation;
pub mod version;
pub mod version_req;
