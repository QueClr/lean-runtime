#include <lean/lean.h>
lean_obj_res probe_decode(uint32_t e, uint8_t with_path, b_lean_obj_arg p) {
    return lean_decode_io_error((int)e, with_path ? p : NULL);
}
lean_obj_res probe_lossy(b_lean_obj_arg a) {
    return lean_mk_string_from_bytes((char const *)lean_sarray_cptr(a), lean_sarray_size(a));
}
uint8_t probe_would_overflow(size_t n) {
    return lean_alloc_sarray_would_overflow(1, n);
}
