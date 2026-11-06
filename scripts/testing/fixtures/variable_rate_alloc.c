/* Test-only native allocation tracking and an independent full-source oracle. */
#include <assert.h>
#include <malloc.h>
#include <stdint.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include "samplerate.h"

void *__real_malloc(size_t);
void *__real_calloc(size_t, size_t);
void *__real_realloc(void *, size_t);
void __real_free(void *);
struct probe_stats { size_t live, usable, allocations, frees, calls, last_input; };
struct record { void *pointer; size_t bytes, usable; };
static _Thread_local struct record records[1024];
static _Thread_local struct probe_stats stats;
static _Thread_local int active;
static _Thread_local size_t requested;
static _Thread_local struct record constructed[3];
size_t probe_requested_bytes(void) { return requested; }
size_t probe_alloc_count(void) { return stats.allocations; }
void probe_constructed(void) {
    assert(!active);size_t n=0;
    for(size_t i=0;i<1024;i++) if(records[i].pointer) {
        assert(n<3);constructed[n++]=records[i];
        stats.live-=records[i].bytes;stats.usable-=records[i].usable;
        records[i].pointer=NULL;
    }
    assert(n==3);
}
void probe_begin(void) { assert(!active);active=1;stats.calls++; }
void probe_end(void) { assert(active);active=0; }
struct probe_stats probe_counts(void) { return stats; }
int probe_process(SRC_STATE *owner,SRC_DATA *data) {
    assert(active && data->input_frames>=0);
    stats.last_input=(size_t)data->input_frames;
    return src_process(owner,data);
}
static size_t find(void *pointer) {
    if(!pointer) return 1024;
    for(size_t i=0;i<1024;i++) if(records[i].pointer==pointer) return i;
    abort();
}
static void forget(size_t i) {
    if(i==1024) return;
    stats.live-=records[i].bytes;stats.usable-=records[i].usable;
    records[i].pointer=NULL;stats.frees++;
}
static void remember(void *pointer,size_t bytes) {
    if(!pointer) return;
    for(size_t i=0;i<1024;i++) if(!records[i].pointer) {
        records[i]=(struct record){pointer,bytes,malloc_usable_size(pointer)};
        stats.live+=bytes;stats.usable+=records[i].usable;stats.allocations++;requested+=bytes;return;
    }
    abort();
}
void *__wrap_malloc(size_t bytes) { void *p=__real_malloc(bytes);if(active) remember(p,bytes);return p; }
void *__wrap_calloc(size_t n,size_t bytes) {
    if(n && bytes>SIZE_MAX/n) return NULL;
    void *p=__real_calloc(n,bytes);if(active) remember(p,n*bytes);return p;
}
void *__wrap_realloc(void *p,size_t bytes) {
    size_t i=active?find(p):1024;void *q=__real_realloc(p,bytes);
    if(active && (q||!bytes)) { forget(i);remember(q,bytes); }return q;
}
/* Test-only fixed watches follow the three actual constructor allocations.
 * Claim/publish keep each slot private through real free and its callback. */
typedef void (*free_observer)(void *,size_t);
static struct { _Atomic(uintptr_t) pointer;size_t bytes;free_observer observer;void *context; } watches[64];
void probe_watch_constructed(free_observer observer,void *context) {
    assert(observer);
    for(size_t j=0;j<3;j++) {
        assert(constructed[j].pointer);size_t i;
        for(i=0;i<64;i++) {uintptr_t empty=0;
            if(atomic_compare_exchange_strong(&watches[i].pointer,&empty,1)) {
                watches[i].bytes=constructed[j].bytes;watches[i].observer=observer;watches[i].context=context;
                atomic_store_explicit(&watches[i].pointer,(uintptr_t)constructed[j].pointer,memory_order_release);break;
            }
        }
        assert(i<64);constructed[j].pointer=NULL;
    }
}
void __wrap_free(void *p) {
    if(active) forget(find(p));
    size_t slot=64;
    if(p) for(size_t i=0;i<64;i++) {uintptr_t expected=(uintptr_t)p;
        if(atomic_compare_exchange_strong_explicit(&watches[i].pointer,&expected,1,memory_order_acquire,memory_order_relaxed)){slot=i;break;}
    }
    __real_free(p);
    if(slot!=64) {watches[slot].observer(watches[slot].context,watches[slot].bytes);atomic_store_explicit(&watches[slot].pointer,0,memory_order_release);}
}

/* Full input + zero extension, one output frame per call. It has no Rust
 * staging/cursor/EOF code and computes its endpoint by integer recurrence. */
size_t probe_reference(const float *input,size_t frames,size_t channels,
                       const uint32_t *rates,size_t rate_count,size_t block,
                       float *output,size_t capacity) {
    assert(!active && frames>0 && channels>0 && rate_count>0 && block>0);
    float *padded=calloc((frames+8192)*channels,sizeof(float));assert(padded);
    memcpy(padded,input,frames*channels*sizeof(float));
    int error=0;SRC_STATE *owner=src_new(SRC_SINC_BEST_QUALITY,(int)channels,&error);
    assert(owner && !error);size_t read=0,written=0;uint64_t cursor=0;
    while(cursor<(uint64_t)frames*1000) {
        assert(written<capacity && read<=frames+8192);
        uint32_t units=rates[(written/block)%rate_count];assert(units>0);
        double ratio=1000.0/units;assert(src_set_ratio(owner,ratio)==0);
        SRC_DATA data={padded+read*channels,output+written*channels,
                       (long)(frames+8192-read),1,0,0,0,ratio};
        assert(src_process(owner,&data)==0 && data.output_frames_gen==1);
        read+=(size_t)data.input_frames_used;written++;cursor+=units;
    }
    assert(src_delete(owner)==NULL);free(padded);return written;
}
