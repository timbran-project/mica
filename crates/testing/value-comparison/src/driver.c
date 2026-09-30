// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later
#define _POSIX_C_SOURCE 200809L
#include "value.c"
#include "allocation.c"
#include "mutex.c"
#include "memory.c"
#include <stdio.h>
#include <time.h>

static void fail(const char *message) { fprintf(stderr, "%s\n", message); exit(2); }
static uint64_t read_number(unsigned bytes) {
    uint64_t result = 0;
    for (unsigned i = 0; i < bytes; ++i) {
        int c = getchar();
        if (c == EOF) fail("truncated test input");
        result |= (uint64_t)(unsigned)c << (i * 8);
    }
    return result;
}
static void write_number(uint64_t n, unsigned bytes) {
    for (unsigned i = 0; i < bytes; ++i) putchar((int)((n >> (i * 8)) & 255));
}
static void *allocate_array(uint64_t count, size_t size) {
    if (count > SIZE_MAX / size) fail("test input size overflow");
    void *p = calloc(count == 0 ? 1 : (size_t)count, size);
    if (p == NULL) fail("test driver allocation failed");
    return p;
}
static uint8_t *read_bytes(uint64_t *length) {
    *length = read_number(8);
    uint8_t *p = allocate_array(*length, 1);
    if (fread(p, 1, (size_t)*length, stdin) != *length) fail("truncated byte input");
    return p;
}
static struct mica_ValueResult read_value(struct mica_MemoryWorker *worker) {
    unsigned tag = (unsigned)read_number(1);
    uint64_t n;
    switch (tag) {
    case 0: return (struct mica_ValueResult){true, 0};
    case 1: return (struct mica_ValueResult){true, mica_value_bool(read_number(1) != 0)};
    case 2: {
        uint64_t bits = read_number(8); int64_t signed_value;
        memcpy(&signed_value, &bits, sizeof(bits));
        return mica_value_int(signed_value);
    }
    case 3: return mica_value_float_from_bits((uint32_t)read_number(4));
    case 4: return mica_value_identity(read_number(8));
    case 5: return mica_value_symbol((uint32_t)read_number(4));
    case 6: return mica_value_error_code((uint32_t)read_number(4));
    case 7: case 8: {
        uint8_t *p = read_bytes(&n);
        struct mica_ValueResult result = tag == 7 ? mica_value_string(worker,p,n) : mica_value_bytes(worker,p,n);
        free(p); return result;
    }
    case 9: case 10: {
        n = read_number(8);
        bool ok = true;
        if (tag == 9) {
            mica_type_Value *values = allocate_array(n,sizeof(*values));
            for (uint64_t i = 0; i < n; ++i) {
                struct mica_ValueResult value = read_value(worker); ok &= value.f_ok; values[i] = value.f_value;
            }
            struct mica_ValueResult result = ok ? mica_value_list(worker,values,n) : (struct mica_ValueResult){false,0};
            free(values); return result;
        }
        struct mica_ValueMapEntry *values = allocate_array(n,sizeof(*values));
        for (uint64_t i = 0; i < n; ++i) {
            struct mica_ValueResult key = read_value(worker), value = read_value(worker);
            ok &= key.f_ok && value.f_ok;
            values[i] = (struct mica_ValueMapEntry){key.f_value,value.f_value};
        }
        struct mica_ValueResult result = ok ? mica_value_map(worker,values,n) : (struct mica_ValueResult){false,0};
        free(values); return result;
    }
    case 11: {
        struct mica_ValueResult start = read_value(worker);
        bool has_end = read_number(1) != 0;
        struct mica_ValueResult end = has_end ? read_value(worker) : (struct mica_ValueResult){true,0};
        if (!start.f_ok || !end.f_ok) return (struct mica_ValueResult){false,0};
        return mica_value_range(worker,start.f_value,has_end,end.f_value);
    }
    case 12: {
        uint32_t code = (uint32_t)read_number(4);
        bool has_message = read_number(1) != 0;
        struct mica_ValueResult message = {true,0};
        if (has_message) {
            uint8_t *p = read_bytes(&n); message = mica_value_string(worker,p,n); free(p);
        }
        bool has_value = read_number(1) != 0;
        struct mica_ValueResult value = has_value ? read_value(worker) : (struct mica_ValueResult){true,0};
        if (!message.f_ok || !value.f_ok) return (struct mica_ValueResult){false,0};
        return mica_value_error(worker,code,has_message,message.f_value,has_value,value.f_value);
    }
    case 13: return mica_value_capability(read_number(8));
    case 14: {
        n = read_number(8); struct mica_ValueResult value = read_value(worker);
        if (!value.f_ok) return value;
        return mica_value_frob(worker,n,value.f_value);
    }
    case 15: return mica_value_function(read_number(8));
    case 16: {
        uint64_t arity=read_number(8);
        uint32_t *heading=allocate_array(arity,sizeof(*heading));
        for(uint64_t i=0;i<arity;i++) heading[i]=(uint32_t)read_number(4);
        n=read_number(8);
        struct mica_ValueTuple *rows=allocate_array(n,sizeof(*rows));
        bool ok=true;
        for(uint64_t i=0;i<n;i++) {
            rows[i].f_arity=read_number(8);
            rows[i].f_data=allocate_array(rows[i].f_arity,sizeof(*rows[i].f_data));
            for(uint64_t j=0;j<rows[i].f_arity;j++) {
                struct mica_ValueResult cell=read_value(worker); ok &= cell.f_ok; rows[i].f_data[j]=cell.f_value;
            }
        }
        struct mica_ValueResult result=ok ? mica_value_relation(worker,heading,arity,rows,n) : (struct mica_ValueResult){false,0};
        for(uint64_t i=0;i<n;i++) free(rows[i].f_data);
        free(rows); free(heading); return result;
    }
    default: fail("unknown test value tag");
    }
    return (struct mica_ValueResult){false,0};
}
static void write_bytes(const uint8_t *p, uint64_t n) {
    write_number(n,8); if (n != 0 && fwrite(p,1,(size_t)n,stdout) != n) fail("output failure");
}
static void write_value(mica_type_Value v) {
    unsigned tag = mica_value_tag(v);
    write_number(tag,1);
    switch (tag) {
    case 0: return;
    case 1: write_number(mica_value_as_bool(v).f_value,1); return;
    case 2: write_number((uint64_t)mica_value_as_int(v).f_number,8); return;
    case 3: {
        float f = mica_value_as_float(v).f_number; uint32_t bits; memcpy(&bits,&f,sizeof(bits)); write_number(bits,4); return;
    }
    case 4: case 13: case 15: write_number(v & UINT64_C(0x00ffffffffffffff),8); return;
    case 5: case 6: write_number(v & UINT64_C(0x00ffffffffffffff),4); return;
    case 7: { const struct mica_HeapString *h = mica_value_as_string(v).f_header; write_bytes(h->f_data,h->f_length); return; }
    case 8: { const struct mica_HeapBytes *h = mica_value_as_bytes(v).f_header; write_bytes(h->f_data,h->f_length); return; }
    case 9: { const struct mica_HeapList *h = mica_value_as_list(v).f_header;
        write_number(h->f_length,8); for (uint64_t i=0;i<h->f_length;++i) write_value(h->f_data[i]); return; }
    case 10: { const struct mica_HeapMap *h = mica_value_as_map(v).f_header;
        write_number(h->f_length,8); for (uint64_t i=0;i<h->f_length;++i) { write_value(h->f_data[i].f_key); write_value(h->f_data[i].f_value); } return; }
    case 11: { const struct mica_HeapRange *h = mica_value_as_range(v).f_header;
        write_value(h->f_start); write_number(h->f_has_end,1); if(h->f_has_end) write_value(h->f_end); return; }
    case 12: { const struct mica_HeapError *h = mica_value_as_error(v).f_header;
        write_number(h->f_code,4); write_number(h->f_has_message,1);
        if(h->f_has_message) { const struct mica_HeapString *s=mica_value_as_string(h->f_message).f_header; write_bytes(s->f_data,s->f_length); }
        write_number(h->f_has_value,1); if(h->f_has_value) write_value(h->f_value); return; }
    case 14: { const struct mica_HeapFrob *h = mica_value_as_frob(v).f_header;
        write_number(h->f_delegate,8); write_value(h->f_value); return; }
    case 16: {
        struct mica_HeapRelation r=mica_value_relation_view(v).f_relation;
        write_number(r.f_arity,8);
        for(uint64_t i=0;i<r.f_arity;i++) write_number(r.f_heading[i],4);
        write_number(r.f_length,8);
        for(uint64_t i=0;i<r.f_length;i++) {
            write_number(r.f_rows[i].f_arity,8);
            for(uint64_t j=0;j<r.f_rows[i].f_arity;j++) write_value(r.f_rows[i].f_data[j]);
        }
        return;
    }
    default: fail("unsupported output kind");
    }
}
struct Prepared {
    unsigned op; struct mica_ValueResult left, right;
    struct mica_ValueMapEntry *entries; uint64_t count;
    struct mica_MemoryWorker input_worker;
    struct mica_MemoryWorker *owner;
    struct mica_MemoryRoot left_root, right_root;
};
enum OutcomeKind { OUTCOME_VALUE, OUTCOME_ORDER, OUTCOME_HASH };
struct Outcome { enum OutcomeKind kind; bool ok; int64_t comparison; mica_type_Value value; };
static struct Outcome run(const struct Prepared *p, struct mica_MemoryWorker *worker) {
    if (!p->left.f_ok || !p->right.f_ok) return (struct Outcome){OUTCOME_VALUE,false,0,0};
    mica_type_Value a=p->left.f_value,b=p->right.f_value;
    struct mica_ValueResult result = {false,0};
    switch (p->op) {
    case 0: result=p->left; break;
    case 1: case 2: {
        struct mica_IntResult c=p->op==1 ? mica_value_compare(a,b) : mica_value_language_compare(a,b);
        return (struct Outcome){OUTCOME_ORDER,c.f_ok,c.f_number,0};
    }
    case 3: result=mica_value_checked_add(a,b); break;
    case 4: result=mica_value_checked_sub(a,b); break;
    case 5: result=mica_value_checked_mul(a,b); break;
    case 6: result=mica_value_checked_div(a,b); break;
    case 7: result=mica_value_checked_rem(a,b); break;
    case 8: result=mica_value_map_get(a,b); break;
    case 9: result=mica_value_map(worker,p->entries,p->count); break;
    case 10: {
        struct mica_HeapBytesResult bytes=mica_value_as_bytes(a);
        if(bytes.f_ok) result=mica_value_string(worker,bytes.f_header->f_data,bytes.f_header->f_length);
        break;
    }
    case 11: {
        struct mica_IntResult number=mica_value_as_int(a);
        if(!number.f_ok || number.f_number<0 || (uint64_t)number.f_number>UINT32_MAX) break;
        struct mica_Utf8Encode encoded=mica_utf8_encode((uint32_t)number.f_number);
        if(encoded.f_ok) result=mica_value_string(worker,encoded.f_bytes.elements,encoded.f_width);
        break;
    }
    case 12: {
        struct mica_IdResult length=mica_value_string_length(a);
        if(length.f_ok && length.f_number<=INT64_MAX) result=mica_value_int((int64_t)length.f_number);
        break;
    }
    case 13: case 15: {
        struct mica_IntResult index=mica_value_as_int(b);
        if(!index.f_ok || index.f_number<0) break;
        if(p->op==13) {
            struct mica_RuneResult rune=mica_value_string_scalar_at(a,(uint64_t)index.f_number);
            if(rune.f_ok) result=mica_value_int(rune.f_rune);
        } else {
            struct mica_IdResult offset=mica_value_string_byte_offset(a,(uint64_t)index.f_number);
            if(offset.f_ok && offset.f_number<=INT64_MAX) result=mica_value_int((int64_t)offset.f_number);
        }
        break;
    }
    case 14: {
        struct mica_HeapListResult bounds=mica_value_as_list(b);
        if(!bounds.f_ok || bounds.f_header->f_length!=2) break;
        struct mica_IntResult start=mica_value_as_int(bounds.f_header->f_data[0]);
        struct mica_IntResult end=mica_value_as_int(bounds.f_header->f_data[1]);
        if(start.f_ok && end.f_ok && start.f_number>=0 && end.f_number>=0)
            result=mica_value_string_slice(worker,a,(uint64_t)start.f_number,(uint64_t)end.f_number);
        break;
    }
    case 16: case 17: {
        if(p->op==17) { result=mica_value_string_concat(worker,a,b); break; }
        struct mica_HeapStringResult suffix=mica_value_as_string(b);
        if(suffix.f_ok) result=mica_value_string_append(worker,a,suffix.f_header->f_data,suffix.f_header->f_length);
        break;
    }
    case 18: {
        struct mica_HeapListResult args=mica_value_as_list(b);
        if(!args.f_ok || args.f_header->f_length!=2) break;
        struct mica_IntResult start=mica_value_as_int(args.f_header->f_data[1]);
        if(!start.f_ok || start.f_number<0) break;
        struct mica_IdResult found=mica_value_string_find(a,args.f_header->f_data[0],(uint64_t)start.f_number);
        if(found.f_ok && found.f_number<=INT64_MAX) result=mica_value_int((int64_t)found.f_number);
        break;
    }
    case 19: {
        struct mica_HeapListResult parts=mica_value_as_list(b);
        if(!parts.f_ok) break;
        result=(struct mica_ValueResult){true,a};
        for(uint64_t i=0;i<parts.f_header->f_length && result.f_ok;i++)
            result=mica_value_string_concat(worker,result.f_value,parts.f_header->f_data[i]);
        break;
    }
    case 20: case 25: {
        struct mica_IdResult n=p->op==20 ? mica_value_list_length(a) : mica_value_map_length(a);
        if(n.f_ok && n.f_number<=INT64_MAX) result=mica_value_int((int64_t)n.f_number);
        break;
    }
    case 21: {
        struct mica_IntResult index=mica_value_as_int(b);
        if(index.f_ok && index.f_number>=0) result=mica_value_list_get(a,(uint64_t)index.f_number);
        break;
    }
    case 22: case 24: case 26: {
        struct mica_HeapListResult args=mica_value_as_list(b);
        if(!args.f_ok || args.f_header->f_length!=2) break;
        mica_type_Value first=args.f_header->f_data[0],second=args.f_header->f_data[1];
        if(p->op==26) { result=mica_value_map_set(worker,a,first,second); break; }
        struct mica_IntResult index=mica_value_as_int(first);
        if(!index.f_ok || index.f_number<0) break;
        if(p->op==24) { result=mica_value_list_set(worker,a,(uint64_t)index.f_number,second); break; }
        struct mica_IntResult end=mica_value_as_int(second);
        if(end.f_ok && end.f_number>=0) result=mica_value_list_slice(worker,a,(uint64_t)index.f_number,(uint64_t)end.f_number);
        break;
    }
    case 23: result=mica_value_list_append(worker,a,b); break;
    case 27: {
        struct mica_HeapListResult args=mica_value_as_list(b);
        if(!args.f_ok) break;
        result=(struct mica_ValueResult){true,a};
        for(uint64_t i=0;i<args.f_header->f_length && result.f_ok;i++)
            result=mica_value_list_append(worker,result.f_value,args.f_header->f_data[i]);
        break;
    }
    case 28: case 29: {
        struct mica_IdResult n=p->op==28 ? mica_value_relation_arity(a) : mica_value_relation_length(a);
        if(n.f_ok && n.f_number<=INT64_MAX) result=mica_value_int((int64_t)n.f_number);
        break;
    }
    case 30: {
        struct mica_IdResult symbol=mica_value_as_symbol(b);
        if(!symbol.f_ok) break;
        struct mica_IdResult position=mica_value_relation_column(a,(uint32_t)symbol.f_number);
        if(position.f_ok) result=mica_value_int((int64_t)position.f_number);
        break;
    }
    case 31: case 32: {
        struct mica_IntResult index=mica_value_as_int(b);
        if(!index.f_ok || index.f_number<0) break;
        if(p->op==31) {
            struct mica_IdResult column=mica_value_relation_column_at(a,(uint64_t)index.f_number);
            if(column.f_ok) result=mica_value_symbol((uint32_t)column.f_number);
        } else {
            struct mica_TupleResult row=mica_value_relation_row(a,(uint64_t)index.f_number);
            if(row.f_ok) result=mica_value_list(worker,row.f_tuple.f_data,row.f_tuple.f_arity);
        }
        break;
    }
    case 33: result=(struct mica_ValueResult){true,mica_value_bool(mica_value_is_unit(a))}; break;
    case 34: {
        struct mica_RelationResult view=mica_value_relation_view(a);
        if(view.f_ok) result=mica_value_relation(worker,view.f_relation.f_heading,view.f_relation.f_arity,view.f_relation.f_rows,view.f_relation.f_length);
        break;
    }
    case 35: {
        struct mica_IdResult hash=mica_value_hash(a);
        return (struct Outcome){OUTCOME_HASH,hash.f_ok,0,hash.f_number};
    }
    case 36: result=mica_value_copy(worker,a); break;
    case 37: case 38: {
        struct mica_BoolResult allow=mica_value_as_bool(b);
        struct mica_ValueCodecOptions options={true,allow.f_ok && allow.f_value};
        // ID mode never uses the symbol table.
        if(p->op==37) result=mica_value_encode(worker,NULL,a,options);
        else {
            struct mica_HeapBytesResult bytes=mica_value_as_bytes(a);
            if(bytes.f_ok) result=mica_value_decode_exact(worker,NULL,bytes.f_header->f_data,bytes.f_header->f_length,options);
        }
        break;
    }
    case 39: result=(struct mica_ValueResult){true,mica_value_bool(mica_value_is_persistable(a))}; break;
    default: fail("unknown operation");
    }
    return (struct Outcome){OUTCOME_VALUE,result.f_ok,0,result.f_value};
}
static uint64_t checksum(struct Outcome r) {
    if (!r.ok) return 0;
    if (r.kind==OUTCOME_HASH) return r.value;
    if (r.kind==OUTCOME_ORDER) return (uint64_t)r.comparison;
    switch(mica_value_tag(r.value)) {
    case 7: return 7 ^ mica_value_as_string(r.value).f_header->f_length;
    case 8: return 8 ^ mica_value_as_bytes(r.value).f_header->f_length;
    case 9: return 9 ^ mica_value_as_list(r.value).f_header->f_length;
    case 10: return 10 ^ mica_value_as_map(r.value).f_header->f_length;
    case 11: return 11;
    case 12: return 12;
    case 14: return 14;
    case 16: return 16 ^ mica_value_relation_length(r.value).f_number;
    default: return r.value;
    }
}
static uint64_t now(void) {
    struct timespec t; if(clock_gettime(CLOCK_MONOTONIC,&t)!=0) fail("clock failed");
    return (uint64_t)t.tv_sec*UINT64_C(1000000000)+(uint64_t)t.tv_nsec;
}
#include "symbol_loads.c"

static int symbol_driver(void) {
    uint64_t count = read_number(8);
    struct LoadName *names = allocate_array(count, sizeof(*names));
    struct mica_IdResult *ids = allocate_array(count, sizeof(*ids));
    struct mica_SymbolTable table = {0};
    if (!mica_value_symbol_table_init(&table)) fail("symbol table initialization failed");
    for (uint64_t i = 0; i < count; ++i) names[i].bytes = read_bytes(&names[i].length);
    for (uint64_t i = 0; i < count; ++i)
        ids[i] = mica_value_symbol_intern(&table, names[i].bytes, names[i].length);
    for (uint64_t i = 0; i < count; ++i) {
        write_number(ids[i].f_ok, 1);
        if (ids[i].f_ok) {
            struct mica_SymbolText text = mica_value_symbol_text(&table, (uint32_t)ids[i].f_number);
            if (!text.f_ok) fail("interned symbol has no text");
            write_number(ids[i].f_number, 8);
            write_bytes(text.f_data, text.f_length);
            write_number(text.f_scalars, 8);
            write_number(text.f_ascii, 1);
        }
        free(names[i].bytes);
    }
    mica_value_symbol_table_release(&table);
    free(names); free(ids);
    return ferror(stdout) ? 2 : 0;
}

// Default name mode changes symbol IDs during decoding. Rust checks the
// returned encoding after interning names back into its own namespace.
static void memory_check(bool result) {
    if (!result) fail("memory lifecycle failed");
}

static void worker_init(struct mica_MemoryWorker *worker, struct mica_MemoryHeap *heap) {
    memory_check(mica_memory_worker_init(worker, heap, 65536));
}

static void release_inputs(struct Prepared *prepared) {
    if (!prepared->left_root.f_registration) return;
    memory_check(mica_memory_enter(prepared->owner));
    memory_check(mica_memory_root_pop(prepared->owner, &prepared->right_root));
    memory_check(mica_memory_root_pop(prepared->owner, &prepared->left_root));
    memory_check(mica_memory_leave(prepared->owner));
    if (prepared->owner == &prepared->input_worker)
        memory_check(mica_memory_worker_release(prepared->owner));
}

static int codec_driver(void) {
    struct mica_SymbolTable table={0};
    if(!mica_value_symbol_table_init(&table)) fail("codec symbol table init failed");
    // Force a different ID order from the Rust source namespace.
    const uint8_t prefix[]="pre-existing C name";
    if(!mica_value_symbol_intern(&table,prefix,sizeof(prefix)-1).f_ok) fail("codec preseed failed");
    uint64_t length;
    uint8_t *bytes=read_bytes(&length);
    struct mica_MemoryHeap heap={0};
    struct mica_MemoryWorker decoded={0},encoded={0};
    memory_check(mica_value_heap_init(&heap));
    worker_init(&decoded,&heap); worker_init(&encoded,&heap);
    memory_check(mica_memory_enter(&decoded));
    struct mica_ValueCodecOptions options={false,false};
    struct mica_ValueResult value=mica_value_decode_exact(&decoded,&table,bytes,length,options);
    free(bytes);
    struct mica_MemoryRoot input={0}, output={0};
    memory_check(mica_memory_root_push(&decoded,&input));
    mica_value_root_set(&input,value.f_ok ? value.f_value : 0);
    memory_check(mica_memory_safepoint(&decoded,true));
    value.f_value=mica_value_root_get(&input);
    memory_check(mica_memory_leave(&decoded));
    memory_check(mica_memory_enter(&encoded));
    struct mica_ValueResult result={false,0};
    if(value.f_ok) result=mica_value_encode(&encoded,&table,value.f_value,options);
    memory_check(mica_memory_root_push(&encoded,&output));
    mica_value_root_set(&output,result.f_ok ? result.f_value : 0);
    memory_check(mica_memory_enter(&decoded));
    memory_check(mica_memory_root_pop(&decoded,&input));
    memory_check(mica_memory_leave(&decoded));
    memory_check(mica_memory_worker_release(&decoded));
    memory_check(mica_memory_safepoint(&encoded,true));
    result.f_value=mica_value_root_get(&output);
    mica_value_symbol_table_release(&table);
    write_number(result.f_ok,1);
    if(result.f_ok) {
        const struct mica_HeapBytes *header=mica_value_as_bytes(result.f_value).f_header;
        write_bytes(header->f_data,header->f_length);
    }
    memory_check(mica_memory_root_pop(&encoded,&output));
    memory_check(mica_memory_leave(&encoded));
    memory_check(mica_memory_worker_release(&encoded));
    memory_check(mica_memory_heap_release(&heap));
    return ferror(stdout) ? 2 : 0;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "codec") == 0) return codec_driver();
    if (argc == 2 && strcmp(argv[1], "symbols") == 0) return symbol_driver();
    if (argc == 3 && strcmp(argv[1], "symbol-load") == 0)
        return symbol_load_driver(strtoull(argv[2], NULL, 10));
    uint64_t rounds=argc==2 ? strtoull(argv[1],NULL,10) : 0;
    struct mica_MemoryHeap heap={0};
    struct mica_MemoryWorker inputs={0}, output={0};
    memory_check(mica_value_heap_init(&heap));
    worker_init(&inputs,&heap); worker_init(&output,&heap);
    uint64_t count=read_number(8);
    struct Prepared *cases=allocate_array(count,sizeof(*cases));
    for(uint64_t i=0;i<count;++i) {
        struct Prepared *p=&cases[i];
        p->op=(unsigned)read_number(1);
        p->owner=&inputs;
        if(p->op==36 || p->op==37 || p->op==38) {
            worker_init(&p->input_worker,&heap);
            p->owner=&p->input_worker;
        }
        memory_check(mica_memory_enter(p->owner));
        p->left=read_value(p->owner); p->right=read_value(p->owner);
        memory_check(mica_memory_root_push(p->owner,&p->left_root));
        memory_check(mica_memory_root_push(p->owner,&p->right_root));
        mica_value_root_set(&p->left_root,p->left.f_ok ? p->left.f_value : 0);
        mica_value_root_set(&p->right_root,p->right.f_ok ? p->right.f_value : 0);
        memory_check(mica_memory_safepoint(p->owner,true));
        p->left.f_value=mica_value_root_get(&p->left_root);
        p->right.f_value=mica_value_root_get(&p->right_root);
        if(p->op==9 && p->left.f_ok) {
            struct mica_HeapListResult list=mica_value_as_list(p->left.f_value);
            if(!list.f_ok || list.f_header->f_length%2!=0) {
                p->left.f_ok=false; memory_check(mica_memory_leave(p->owner)); continue;
            }
            p->count=list.f_header->f_length/2; p->entries=allocate_array(p->count,sizeof(*p->entries));
            for(uint64_t j=0;j<p->count;++j) p->entries[j]=(struct mica_ValueMapEntry){list.f_header->f_data[2*j],list.f_header->f_data[2*j+1]};
        }
        memory_check(mica_memory_leave(p->owner));
    }
    memory_check(mica_memory_enter(&output));
    if(rounds==0) {
        for(uint64_t i=0;i<count;++i) {
            struct Outcome r=run(&cases[i],&output);
            struct mica_MemoryRoot result={0};
            memory_check(mica_memory_root_push(&output,&result));
            mica_value_root_set(&result,r.ok && r.kind==OUTCOME_VALUE ? r.value : 0);
            // Copy and codec results survive removal of every source root.
            if(cases[i].owner==&cases[i].input_worker) release_inputs(&cases[i]);
            memory_check(mica_memory_safepoint(&output,true));
            if(r.ok && r.kind==OUTCOME_VALUE) r.value=mica_value_root_get(&result);
            write_number(!r.ok ? 0 : r.kind==OUTCOME_HASH ? 3 : r.kind==OUTCOME_ORDER ? 2 : 1,1);
            if(r.ok) {
                if(r.kind==OUTCOME_ORDER) write_number((uint64_t)r.comparison,8);
                else if(r.kind==OUTCOME_HASH) write_number(r.value,8);
                else write_value(r.value);
            }
            memory_check(mica_memory_root_pop(&output,&result));
        }
    } else {
        // Worker setup and rooted inputs stay outside the timer. Safepoint polls
        // and reclamation of all temporary outputs are included in each sample.
        for(uint64_t i=0;i<count;++i) {
            (void)run(&cases[i],&output);
            memory_check(mica_memory_safepoint(&output,false));
        }
        memory_check(mica_memory_safepoint(&output,true));
        uint64_t digest=0,start=now();
        for(uint64_t round=0;round<rounds;++round) for(uint64_t i=0;i<count;++i) {
            struct Outcome r=run(&cases[i],&output);
            uint64_t observed=checksum(r);
            __asm__ volatile("" : "+r"(observed) : : "memory");
            digest+=observed;
            memory_check(mica_memory_safepoint(&output,false));
        }
        memory_check(mica_memory_safepoint(&output,true));
        uint64_t elapsed=now()-start;
        printf("%llu %llu\n",(unsigned long long)elapsed,(unsigned long long)digest);
    }
    memory_check(mica_memory_leave(&output));
    memory_check(mica_memory_worker_release(&output));
    for(uint64_t i=count;i!=0;--i) {
        release_inputs(&cases[i-1]); free(cases[i-1].entries);
    }
    free(cases);
    memory_check(mica_memory_worker_release(&inputs));
    memory_check(mica_memory_heap_release(&heap));
    return ferror(stdout) ? 2 : 0;
}
