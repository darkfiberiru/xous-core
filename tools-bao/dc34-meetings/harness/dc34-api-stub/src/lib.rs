//! Just enough of `dc34-api` to host the real meeting modules.
pub const DC34_DICT: &str = "dc34";

#[path = "../../../../../libs/dc34-api/src/meeting.rs"]
pub mod meeting;
#[path = "../../../../../libs/dc34-api/src/meeting_store.rs"]
pub mod meeting_store;
