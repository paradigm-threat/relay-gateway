//! Branch feature helpers (`comments`, `dev`) from catalog records.
//! Enforcement hooks live in [`branch_policy`](super::branch_policy).

pub use crate::catalog_policy::encode_comment_target_path;

/// Default comment storage root when `branch_features.comments.path_root` is unset.
pub const DEFAULT_COMMENTS_PATH_ROOT: &str = ".relay/comments/by-path/";

/// Resolve comment file prefix for a published path.
pub fn comment_storage_prefix(path_root: &str, target_path: &str) -> String {
    let root = path_root.trim_end_matches('/');
    format!("{}/{}", root, encode_comment_target_path(target_path))
}
