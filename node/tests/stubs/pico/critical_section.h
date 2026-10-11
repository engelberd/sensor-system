#pragma once

struct critical_section_t {};

inline int test_critical_section_depth = 0;

inline void critical_section_init(critical_section_t*) {}
inline void critical_section_enter_blocking(critical_section_t*) {
    ++test_critical_section_depth;
}
inline void critical_section_exit(critical_section_t*) {
    --test_critical_section_depth;
}
