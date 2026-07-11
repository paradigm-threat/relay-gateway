pub mod binary_policy;
pub mod branch_features;
pub mod branch_policy;
pub mod ipfs;
pub mod clever_hooks;
pub mod clever_script_host;
pub mod embed_html;
pub mod get_hook_http;
pub mod hooks;
pub mod image_cache;
pub mod image_transform;
pub mod index_policy;
pub mod markdown_html;
pub mod meta_index;
pub mod index_store;
pub mod indexing;
pub mod query;
pub mod search_plugins;
pub mod sqlite_index;
pub mod relay_config_cache;
pub mod repo;
pub mod request_headers;
pub mod resolve;
pub mod worktree;

#[cfg(test)]
mod tests;

pub use branch_policy::{
    branch_requires_signature, enforce_all_commits_on_branch, enforce_push_range,
    enforce_push_range_with_catalog, enforce_single_commit, enforce_unsigned_http_allowed,
    resolve_branch_rule,
};
pub use get_hook_http::get_hook_json_to_response;
pub use hooks::{execute_repo_hook, HookContext};
pub use indexing::{
    branch_data_dir, branch_hash_short, branch_index_paths, branch_operation_mutex,
    branch_sqlite_path, ensure_indexed,
};
pub use repo::{
    apply_bare_receive_security, apply_bare_receive_security_at_path, bare_repo_names,
    default_branch_from_bare_head, get_branch_commit_info, list_branches, open_repo,
    open_repo_for_state, read_file_from_repo, read_git_config, read_git_config_from_revision,
    read_relay_config, repo_names_for_state,
};
pub use resolve::git_resolve_and_respond;
