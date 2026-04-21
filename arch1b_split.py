#!/usr/bin/env python3
"""ARCH-1B: Split src/refresh/mod.rs into focused sub-modules."""

import re
import os

filepath = 'src/refresh/mod.rs'
if not os.path.exists(filepath):
    print(f"Error: {filepath} not found.")
    exit(1)

with open(filepath, 'r') as f:
    content = f.read()
    lines = content.splitlines(keepends=True)

total = len(lines)
print(f"Total lines in mod.rs: {total}")

# Section boundaries
CODEGEN_START = 189 - 1  # 0-indexed
CODEGEN_END = 2528       # exclusive

ORCH_START1 = 2529 - 1
ORCH_END1 = 2656

MERGE_START = 2657 - 1
MERGE_END = 5956

ORCH_START2 = 5957 - 1
ORCH_END2 = 6201

MERGE_EXTRA_START = 6202 - 1
MERGE_EXTRA_END = 6264

TESTS_START = 6265 - 1
TESTS_END = total

COMMON_IMPORTS = '''#![allow(clippy::too_many_arguments)]

use pgrx::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Instant;

use crate::catalog::{StDependency, StreamTableMeta};
use crate::dag::RefreshMode;
usususususususususususususususus::ususususususususususurausususususususususususususususus::ususususususususususurausususu''
ususususrite codegen.ususususrite codegen.usususus(liususususrite codegen.usususus])
cccccccccccccccccccccccccccccccccccccccccc-generation sub-module for the refresh pipeline.
{COMMON_IMPORTS}
{codegen_body}'''
with open('src/refresh/codegen.rs', 'w') as f: f.write(codegen_content)

# --- Write merge.rs# --- Write merge= ''.join(lines[MERGE_START:MERGE_END])
mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmXTRAmmmmmmmmmmmmmmmmmmmmmD]mmmmmme_content = f'''// ARCH-1B: MERGE execution sub-module for the refresh pipelmmmmmmmmmmmmmmmPORTS}
{merge_body1}
{merge_body{merge_body{mer('src/ref{merge_body{merge_b) as{merge_body{merge_content)

# --- Write orchestrator.rs ---
orch_body1 = ''.joorch_body1 = ''.joo1:ORCH_END1])
orch_body2 = ''.join(orch_body2 = ''.join(orch_bod
orch_content = f'''// ARCH-1B: Orchestration sub-module for the refresh pipeline.
{COMMON_IMPORTS}
{orch_body1}
{orch_body2}'''
with open('src/refresh/orchestrator.rs', 'w') as f: f.write(orch_content)

# --- Write tests.rs ---
tests_body = ''.join(lines[TESTS_STtests_body = ''.join(lines[TESTS_STtests_body = ''.join(lines[TESTS_STtests_body = .
tests_body = ''.join(lines[TESTS_STtests_body = ''.join(lines[TESTS_STtests_body = '' crate::version::Frontier;
{tests_body}''{tests_body}''{tests_body}''{tests_body}''{tests_body}''{tests_body}''
print("Verification:")
for fname in ['codegen.rs', 'merge.rs', 'orchestrator.rs', 'tests.rs']:
    p = f'src/refresh/{fname}'
    if os.path.exists(p):
        with open(p) as fh: print(f"  {p}: {len(fh.readlines())} lines")
    else: print(f"  {p}: NOT FOUND")
