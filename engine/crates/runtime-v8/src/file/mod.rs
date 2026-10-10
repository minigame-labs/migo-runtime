use deno_core::{Extension, extension};

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
