#!/usr/bin/env node
/** Phase A 保存則回帰: Raw非破壊 / CREATED / current一意 / concurrent supersede */
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { DatabaseSync } from "node:sqlite";
import fs from "node:fs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const WS = "phase-a-test";
const dbPath = join(ROOT, "stores", WS, "store.db");
for (const p of [dbPath, dbPath + "-wal", dbPath + "-shm"]) try { fs.rmSync(p, { force: true }); } catch {}

const { Client } = await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","index.js")).href);
const { StdioClientTransport } = await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","stdio.js")).href);
const text = r => (r.content ?? []).map(x=>x.text).join("\n");
let pass=0, fail=0;
const check=(n,ok,d="")=>{ if(ok){pass++;console.log(`PASS ${n}`)}else{fail++;console.error(`FAIL ${n}${d?` — ${d}`:""}`)}};
async function open(name="phase-a") { const c=new Client({name,version:"1"}); await c.connect(new StdioClientTransport({command:process.execPath,args:[join(ROOT,"bin","ant-memory.mjs")],cwd:ROOT,env:{...process.env,ANT_MEMORY_ROOT:ROOT,ANT_MEMORY_WORKSPACE:WS,ANT_MEMORY_CLIENT:name}})); return c; }
const idOf=s=>s.match(/id=`([0-9a-f]{10})`/i)?.[1];
const call=(c,name,args)=>c.callTool({name,arguments:args});

const a=await open("phase-a-A");
let b;
try {
  const w=text(await call(a,"write_context",{topic:"Phase A created",content:"CREATED_EVENT_PROBE",ttl_days:0.000001}));
  const id=idOf(w); check("通常writeがIDを返す",!!id,w);
  await new Promise(r=>setTimeout(r,150));
  await a.close();

  const a2=await open("phase-a-A");
  const raw=text(await call(a2,"read_context",{id}));
  check("TTL失効後・再接続後もRawが残る",raw.includes("CREATED_EVENT_PROBE"),raw);
  const hist=text(await call(a2,"get_context_history",{id}));
  check("通常writeにCREATED eventがある",/\[created\]/.test(hist),hist);
  check("TTL失効はexpired状態になる",/状態: expired/.test(hist),hist);

  const missing=text(await call(a2,"restore_context",{id:"0000000000",reason:"missing must fail"}));
  check("存在しないIDのrestoreを拒否",/見つかりません/.test(missing),missing);

  const base=idOf(text(await call(a2,"write_context",{topic:"Phase A base",content:"BASE"})));
  const sup=text(await call(a2,"supersede_context",{old_id:base,topic:"Phase A audit",content:"Audit",reason:"first revision"}));
  const audit=idOf(sup); check("最初のsupersede成功",!!audit,sup);
  const restoreOld=text(await call(a2,"restore_context",{id:base,reason:"must not create two current"}));
  check("後継のある旧版をcurrentへrestore拒否",/後継/.test(restoreOld),restoreOld);
  const stale=text(await call(a2,"supersede_context",{old_id:base,topic:"Phase A stale",content:"STALE",reason:"must fail"}));
  check("superseded旧版から再分岐を拒否",/active ではありません|競合/.test(stale),stale);

  const conc=idOf(text(await call(a2,"write_context",{topic:"Phase A concurrent",content:"CONCURRENT"})));
  b=await open("phase-a-B");
  const [r1,r2]=await Promise.all([
    call(a2,"supersede_context",{old_id:conc,topic:"Concurrent A",content:"A",reason:"race A"}),
    call(b,"supersede_context",{old_id:conc,topic:"Concurrent B",content:"B",reason:"race B"}),
  ]);
  const outs=[text(r1),text(r2)];
  check("同時supersedeは片方だけ成功",outs.filter(x=>/置換完了/.test(x)).length===1,outs.join(" || "));
  check("同時supersedeの片方は競合拒否",outs.filter(x=>/競合|active ではありません/.test(x)).length===1,outs.join(" || "));

  const db=new DatabaseSync(dbPath,{readOnly:true});
  const successors=db.prepare("SELECT COUNT(*) c FROM memory_relations WHERE target_id=? AND relation_type='supersedes'").get(conc).c;
  const orphan=db.prepare("SELECT COUNT(*) c FROM memory_lifecycle l LEFT JOIN chunks c ON c.id=l.chunk_id WHERE c.id IS NULL").get().c;
  check("旧版ごとのsupersedes後継は最大1",successors===1,`successors=${successors}`);
  check("存在しないrestoreでlifecycle孤児を作らない",orphan===0,`orphans=${orphan}`);
  db.close();
  await a2.close();
} finally { try{await a.close()}catch{} try{await b?.close()}catch{} }
console.log(`\n結果: ${pass} passed / ${fail} failed`);
process.exit(fail?1:0);
