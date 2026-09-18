// The numbers service ops travel under on the service stream.
//
// contracts/runtime/service-ops.json is the source. This copy is checked
// against it entry for entry when the engine is staged
// (scripts/gen-performance-plus-engine.py), and the host's copy by a Rust test,
// so the three cannot disagree without a build failing. Append only.

export const SERVICE_OP = Object.freeze({
  op_storage_get: 1,
  op_storage_set: 2,
  op_storage_remove: 3,
  op_storage_clear: 4,
  op_storage_info: 5,
  op_storage_get_async: 6,
  op_storage_set_async: 7,
  op_storage_remove_async: 8,
  op_storage_clear_async: 9,
  op_storage_info_async: 10,
  op_create_buffer_url: 11,
  op_revoke_buffer_url: 12,
});
