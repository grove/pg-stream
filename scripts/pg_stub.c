/*
 * pg_stub.c — Stub definitions for PostgreSQL server symbols.
 *
 * On macOS 26+ (Tahoe), dyld eagerly resolves all flat namespace symbols at
 * load time.  pgrx extensions reference ~28 PostgreSQL server-internal symbols
 * (MemoryContexts, SPI functions, etc.) that are normally provided by the
 * `postgres` executable when the extension is loaded as a shared library.
 *
 * When we run `cargo test --lib` to execute pure-Rust unit tests, there is no
 * postgres process — so those symbols are undefined and dyld aborts with:
 *
 *     dyld: symbol not found in flat namespace '_CacheMemoryContext'
 *
 * This stub provides NULL / no-op definitions for every PostgreSQL symbol the
 * test binary references.  It is compiled into `libpg_stub.dylib` and injected
 * via DYLD_INSERT_LIBRARIES when running unit tests.
 *
 * IMPORTANT: None of these stubs are ever _called_ during unit tests — the
 * tests exercise pure Rust logic only.  If a test accidentally calls a PG
 * function it will get a NULL pointer / zero return, which will crash or
 * fail the test (the desired behaviour).
 *
 * Regenerate the symbol list with:
 *   nm target/debug/deps/pg_trickle-* | grep ' U _' | awk '{print $NF}' \
 *     | grep -E '^_(Alloc|Cache|Copy|Cur|Current|err|Error|format_type|Free|Get|Is|Mem|Message|PG_|pfree|Portal|Postmaster|SPI_|Top)'
 */

#include <stddef.h>
#include <stdint.h>

/* ── MemoryContext globals (all NULL) ─────────────────────────────────── */
void *CacheMemoryContext        = NULL;
void *CurrentMemoryContext      = NULL;
void *CurTransactionContext     = NULL;
void *ErrorContext              = NULL;
void *MessageContext            = NULL;
void *PortalContext             = NULL;
void *PostmasterContext         = NULL;
void *TopMemoryContext          = NULL;
void *TopTransactionContext     = NULL;

/* ── Error handling globals ───────────────────────────────────────────── */
void *error_context_stack       = NULL;
void *PG_exception_stack        = NULL;

/* ── Memory allocation functions ──────────────────────────────────────── */
void *palloc0(size_t size)                      { (void)size; return NULL; }

/* ── MemoryContext functions ──────────────────────────────────────────── */
void *AllocSetContextCreateInternal(void *parent, const char *name,
                                    size_t minContextSize,
                                    size_t initBlockSize,
                                    size_t maxBlockSize) {
    (void)parent; (void)name;
    (void)minContextSize; (void)initBlockSize; (void)maxBlockSize;
    return NULL;
}

void  MemoryContextDelete(void *ctx)            { (void)ctx; }
void *MemoryContextGetParent(void *ctx)         { (void)ctx; return NULL; }
void  pfree(void *ptr)                          { (void)ptr; }

/* ── Error data ──────────────────────────────────────────────────────── */
void *CopyErrorData(void)                       { return NULL; }
void  FreeErrorData(void *edata)                { (void)edata; }

/* ── Error reporting functions ───────────────────────────────────────── */
int   errcode(int sqlerrcode)                   { (void)sqlerrcode; return 0; }
int   errmsg(const char *fmt, ...)              { (void)fmt; return 0; }
int   errdetail(const char *fmt, ...)           { (void)fmt; return 0; }
int   errhint(const char *fmt, ...)             { (void)fmt; return 0; }
int   errcontext_msg(const char *fmt, ...)      { (void)fmt; return 0; }
int   errstart(int elevel, const char *domain)  { (void)elevel; (void)domain; return 0; }
void  errfinish(const char *filename, int lineno, const char *funcname) {
    (void)filename; (void)lineno; (void)funcname;
}

/* ── Transaction / type helpers ───────────────────────────────────────── */
uint32_t GetCurrentTransactionIdIfAny(void)     { return 0; }
int   IsBinaryCoercible(uint32_t a, uint32_t b) { (void)a; (void)b; return 0; }
char *format_type_extended(uint32_t oid, int32_t typmod, int flags) {
    (void)oid; (void)typmod; (void)flags;
    return NULL;
}

/* ── SPI functions ───────────────────────────────────────────────────── */
int      SPI_connect(void)                      { return -1; }
int      SPI_finish(void)                       { return -1; }
int      SPI_execute(const char *cmd, int ro, long cnt) {
    (void)cmd; (void)ro; (void)cnt;
    return -1;
}
int      SPI_execute_with_args(const char *cmd, int nargs,
                               void *argtypes, void *values,
                               const char *nulls, int ro, long cnt) {
    (void)cmd; (void)nargs; (void)argtypes; (void)values;
    (void)nulls; (void)ro; (void)cnt;
    return -1;
}
void    *SPI_getbinval(void *tuple, void *tupdesc, int fnumber,
                       int *isnull) {
    (void)tuple; (void)tupdesc; (void)fnumber;
    if (isnull) *isnull = 1;
    return NULL;
}
uint32_t SPI_gettypeid(void *tupdesc, int fnumber) {
    (void)tupdesc; (void)fnumber;
    return 0;
}

/* SPI globals */
uint64_t SPI_processed = 0;
void    *SPI_tuptable  = NULL;

/* ── sigsetjmp stub ──────────────────────────────────────────────────── */
/*
 * pgrx's PG_exception_stack references sigsetjmp indirectly. On macOS the
 * symbol is provided by libSystem so it should always resolve, but we
 * include it here defensively.
 */
