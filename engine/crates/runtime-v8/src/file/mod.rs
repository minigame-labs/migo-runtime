use std::{cell::RefCell, rc::Rc};

use deno_core::{Extension, OpState, extension, op2};
use deno_error::JsErrorBox;
use shared::{op_state::HostOpState, services::host_files::Step};

use crate::file::fs::{
    op_access, op_access_sync, op_close_file, op_close_file_sync, op_copy_file, op_copy_file_sync,
    op_fstat, op_fstat_sync, op_ftruncate, op_ftruncate_sync, op_get_file_info,
    op_get_file_info_sync, op_list_saved_files, op_mkdir, op_mkdir_sync, op_open_file,
    op_open_file_sync, op_read_compressed_file, op_read_compressed_file_sync, op_read_fd,
    op_read_fd_into, op_read_fd_into_sync, op_read_fd_sync, op_read_file, op_read_file_sync,
    op_read_zip_entry, op_readdir, op_readdir_sync, op_rename, op_rename_sync, op_rmdir,
    op_rmdir_sync, op_stat, op_stat_sync, op_unlink, op_unlink_sync, op_unzip, op_write_file,
    op_write_file_sync, op_write_or_append_file, op_write_or_append_file_sync,
};

mod fs;
pub(crate) mod host_paths;

/// `saveFileToDisk`: the file to save, which must be content's own.
const SAVE_FILE_FILES: &[&[Step]] = &[&[Step::Key("filePath")]];

/// Save a file of content's to the player's disk, through the host's save
/// dialog. The path crosses as a real one the host can read while the request
/// is open; a remote URL is not a file content has.
#[op2]
pub async fn op_save_file_to_disk(
    state: Rc<RefCell<OpState>>,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let service = state
        .borrow()
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.image_api())
        .ok_or_else(|| JsErrorBox::generic("saveFileToDisk:fail not supported"))?;
    let request = host_paths::export_paths(
        &state,
        &request_json,
        SAVE_FILE_FILES,
        host_paths::Remote::Refused,
    )
    .await?;
    service
        .save_file_to_disk(&request)
        .map_err(JsErrorBox::generic)
}

extension!(host_v8_file,
    deps = [host_v8_console, host_v8_base, host_v8_io_state],
    ops = [
        op_access,
        op_access_sync,
        op_write_or_append_file,
        op_write_or_append_file_sync,
        op_open_file,
        op_open_file_sync,
        op_close_file,
        op_close_file_sync,
        op_copy_file,
        op_copy_file_sync,
        op_fstat,
        op_fstat_sync,
        op_ftruncate,
        op_ftruncate_sync,
        op_mkdir,
        op_mkdir_sync,
        op_readdir,
        op_readdir_sync,
        op_unlink,
        op_unlink_sync,
        op_rename,
        op_rename_sync,
        op_rmdir,
        op_rmdir_sync,
        op_stat,
        op_stat_sync,
        op_write_file,
        op_write_file_sync,
        op_read_fd,
        op_read_fd_sync,
        op_read_fd_into,
        op_read_fd_into_sync,
        op_read_compressed_file,
        op_read_compressed_file_sync,
        op_read_file,
        op_read_file_sync,
        op_read_zip_entry,
        op_unzip,
        op_get_file_info,
        op_get_file_info_sync,
        op_list_saved_files,
        op_save_file_to_disk,
    ],
    esm = [
        dir "src/file",
        "01_save.js",
        "02_file_stats.js",
        "02_file_manager.js",
    ],
);

pub fn file_extensions() -> Vec<Extension> {
    vec![host_v8_file::init()]
}

pub fn file_lazy_extensions() -> Vec<Extension> {
    vec![host_v8_file::lazy_init()]
}
