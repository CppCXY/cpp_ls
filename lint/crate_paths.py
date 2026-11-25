#!/usr/bin/env python3
"""Keep references short: reject/fix `crate::path` in code, leaving `use` lines alone.

Usage:
    python lint/crate_paths.py check [--root .]
    python lint/crate_paths.py fix   [--root .]

The check is meant for CI. The fix is deliberately conservative: it imports the first
segment (`crate::module` -> `module`) once per lexical module scope, skips names that
already exist in that file, and never edits a `use` declaration. Run cargo fmt/check after it.
"""
from __future__ import annotations
import argparse, pathlib, re, subprocess, sys

SKIP_PARTS = {"target", ".git", ".cppls"}
USE = re.compile(r"^\s*(?:pub\s+)?use\b")
MOD = re.compile(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{")
PATH = re.compile(r"\bcrate::([A-Za-z_][A-Za-z0-9_]*)")

def masked(text: str) -> str:
    out=[]; i=0; block=0; line=False; quote=None; raw_hashes=0
    while i < len(text):
        c=text[i]
        if line:
            if c=='\n': line=False; out.append(c)
            else: out.append(' ')
            i+=1; continue
        if block:
            if text.startswith('/*',i): block+=1; out.extend('  '); i+=2; continue
            if text.startswith('*/',i): block-=1; out.extend('  '); i+=2; continue
            out.append('\n' if c=='\n' else ' '); i+=1; continue
        if quote:
            if c=='\\': out.extend('  '); i+=2; continue
            if c==quote: quote=None
            out.append('\n' if c=='\n' else ' '); i+=1; continue
        if text.startswith('//',i): line=True; out.extend('  '); i+=2; continue
        if text.startswith('/*',i): block=1; out.extend('  '); i+=2; continue
        if c=='r' and text.startswith('r#"',i) or c=='r' and text.startswith('r"',i):
            # raw strings, including hashes
            j=i+1; hashes=0
            while j < len(text) and text[j]=='#': hashes+=1; j+=1
            if j < len(text) and text[j]=='"':
                end=text.find('"'+'#'*hashes, j+1)
                if end < 0: break
                seg=text[i:end+1+hashes]
                out.extend('\n' if ch=='\n' else ' ' for ch in seg); i=end+1+hashes; continue
        if c in ('"',"'"): quote=c; out.append(' '); i+=1; continue
        out.append(c); i+=1
    return ''.join(out)

def scopes(m: str):
    """(start_line, end_line, indent) for each inline module, 0 = file."""
    stack=[(0,len(m.splitlines()),0)]; depth=0; cur=[]
    lines=m.split('\n')
    for no,line in enumerate(lines,1):
        mm=MOD.match(line)
        if mm:
            indent=len(line)-len(line.lstrip())
            stack.append((no,len(lines),indent)); continue
        depth += line.count('{')-line.count('}')
    # approximate end by matching from module start
    result=[(0,len(lines),0)]
    for idx in range(1,len(stack)):
        start,_,indent=stack[idx]; d=0; end=len(lines)
        for no in range(start,len(lines)+1):
            d += lines[no-1].count('{')-lines[no-1].count('}')
            if d==0: end=no; break
        result.append((start,end,indent))
    return result

def file_info(path: pathlib.Path):
    text=path.read_text(encoding='utf-8'); m=masked(text); lines=text.split('\n'); ml=m.split('\n')
    scopes_list=scopes(m)
    def owner(no):
        best=scopes_list[0]
        for s in scopes_list:
            if s[0] <= no <= s[1] and s[0] >= best[0]: best=s
        return best
    occurrences=[]
    in_use=False
    for no,line in enumerate(ml,1):
        s=line.strip()
        if in_use or USE.match(line):
            if ';' in line: in_use=False
            elif USE.match(line): in_use=True
            continue
        for mm in PATH.finditer(line):
            occurrences.append((no,mm.start(),mm.group(1),owner(no)))
    return text,lines,occurrences

def existing_names(text):
    names=set()
    for line in masked(text).split('\n'):
        mm=re.match(r"^\s*(?:pub\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)",line)
        if mm: names.add(mm.group(1))
    # Parse use declarations, including multiline groups.
    joined=' '.join(masked(text).splitlines())
    for m in re.finditer(r"\buse\s+([^;]+);",joined):
        body=m.group(1)
        prefix=''
        if '{' in body:
            prefix=body.split('{',1)[0]
        for item in re.split(r",",body):
            item=item.strip()
            if item in ('','{','}'): continue
            if item.endswith('{'): continue
            item=item.rstrip('}').strip()
            if ' as ' in item: item=item.split(' as ')[-1].strip()
            if item=='self':
                parts=prefix.rstrip(':').split('::')
                if parts: names.add(parts[-1])
            else:
                name=splits=item.split('::')[-1].strip()
                if name: names.add(name)
    return names

def check(paths):
    total=0; files=0
    for p in paths:
        text,lines,occ=file_info(p)
        if occ:
            files+=1; total+=len(occ)
            for no,_c,seg,sc in occ[:5]:
                print(f"{p}:{no}: crate::{seg} (scope starts line {sc[0]})")
            if len(occ)>5: print(f"{p}: ... {len(occ)-5} more")
    print(f"crate paths: {total} occurrence(s) in {files} file(s)")
    return 1 if total else 0

def fix(paths, verify=True):
    backups={p:p.read_text(encoding="utf-8") for p in paths}
    changed=0; skipped=0
    for p in paths:
        text,lines,occ=file_info(p)
        if not occ: continue
        existing=existing_names(text)
        by_scope={}
        for no,col,seg,sc in occ:
            key=(sc[0],sc[2])
            by_scope.setdefault(key,{}).setdefault(seg,set()).add(no)
        imports=[]; replace=set()
        for (start,indent),segs in by_scope.items():
            for seg in segs:
                replace.add(seg)
                if seg not in existing:
                    imports.append((start,indent,seg))
        if not imports:
            skipped+=len(occ); continue
        # replace only code lines, not use/comment/string occurrences (masked line says code)
        ml=masked(text).split('\n')
        for no,line in enumerate(lines):
            if no < len(ml) and USE.match(ml[no]): continue
            new=line
            for seg in sorted(replace,key=len,reverse=True):
                new=new.replace('crate::'+seg,seg)
            lines[no]=new
        # insert imports, bottom scope first (line shifts)
        imports.sort(key=lambda x:(x[0],x[1]), reverse=True)
        inserted = set()
        for _start, _indent, seg in imports:
            if seg in inserted:
                continue
            inserted.add(seg)
            idx=0
            while idx<len(lines) and (lines[idx].startswith('//!') or lines[idx].startswith('#![') or lines[idx].strip()==''):
                idx+=1
            while idx<len(lines) and (lines[idx].lstrip().startswith('extern crate') or lines[idx].lstrip().startswith('#[macro_use]')):
                idx+=1
            lines.insert(idx,f'use crate::{seg};')
        p.write_text('\n'.join(lines),encoding='utf-8'); changed+=1
    print(f"fixed {changed} file(s), skipped {skipped} conflicting/ambiguous occurrence(s)")
    if verify and changed:
        result=subprocess.run(["cargo","check","--workspace","--all-targets"],cwd=pathlib.Path(__file__).resolve().parent.parent)
        if result.returncode!=0:
            for p,text in backups.items(): p.write_text(text,encoding="utf-8")
            print("verification failed; all changes were reverted")
            return result.returncode
    return 0

def main():
    ap=argparse.ArgumentParser(); ap.add_argument('mode',choices=['check','fix']); ap.add_argument('--root',default='.')
    ap.add_argument('--no-verify',action='store_true')
    ap.add_argument('--changed',action='store_true')
    args=ap.parse_args(); root=pathlib.Path(args.root)
    if args.changed:
        import subprocess
        try:
            names=subprocess.check_output(['git','diff','--name-only','HEAD'],text=True).splitlines()
            if not names:
                names=subprocess.check_output(['git','diff','--name-only','HEAD^','HEAD'],text=True).splitlines()
        except Exception:
            names=[]
        paths=[root/n for n in names if n.endswith('.rs') and (root/n).is_file()]
    else:
        paths=[p for p in root.rglob('*.rs') if not (set(p.parts)&SKIP_PARTS)]
    return check(paths) if args.mode=='check' else fix(paths, verify=not args.no_verify)
if __name__=='__main__': sys.exit(main())
