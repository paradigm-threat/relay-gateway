pub mod repo;
pub mod resolve;
pub mod hooks;
pub mod indexing;
pub mod query;
pub mod branch_policy;

#[cfg(test)]
mod tests;

pub use repo::{
    apply_bare_receive_security,
    apply_bare_receive_security_at_path,
    bare_repo_names, default_branch_from_bare_head, get_branch_commit_info, list_branches,
    open_repo, read_file_from_repo, read_git_config, read_git_config_from_revision, read_relay_config,
};
pub use resolve::git_resolve_and_respond;
pub use hooks::{execute_repo_hook, HookContext};
pub use indexing::ensure_indexed;
pub use branch_policy::{
    branch_requires_signature, enforce_push_range, enforce_unsigned_http_allowed, resolve_branch_rule,
};
