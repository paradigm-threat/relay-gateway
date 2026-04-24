pub mod branch_policy;
pub mod hooks;
pub mod index_policy;
pub mod index_store;
pub mod indexing;
pub mod query;
pub mod repo;
pub mod resolve;

#[cfg(test)]
mod tests;

pub use branch_policy::{
    branch_requires_signature, enforce_push_range, enforce_unsigned_http_allowed,
    resolve_branch_rule,
};
pub use hooks::{execute_repo_hook, HookContext};
pub use indexing::{
    branch_data_dir, branch_hash_short, branch_index_paths, branch_operation_mutex, ensure_indexed,
};
pub use repo::{
    apply_bare_receive_security, apply_bare_receive_security_at_path, bare_repo_names,
    default_branch_from_bare_head, get_branch_commit_info, list_branches, open_repo,
    read_file_from_repo, read_git_config, read_git_config_from_revision, read_relay_config,
};
pub use resolve::git_resolve_and_respond;
