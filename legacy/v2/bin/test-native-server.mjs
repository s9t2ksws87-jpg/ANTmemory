#!/usr/bin/env node
/** Phase C1 + post-D cleanup: native ownership and simplified default surface. */
import { spawnSync } from "node:child_process";
import { readFileSync, rmSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const WS = "phase-c1-test";
const db = join(ROOT, "stores", WS, "store.db");
for (const p of [db, db + "-wal", db + "-shm"]) try { rmSync(p, { force: true }); } catch {}

const { Client } = await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","index.js")).href);
const { StdioClientTransport } = await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","stdio.js")).href);
let pass=0, fail=0;
const check=(n,ok,d="")=>{ if(ok){pass++;console.log(`PASS ${n}`)}else{fail++;console.error(`FAIL ${n}${d?` — ${d}`:""}`)}};
const text=r=>(r.content??[]).map(x=>x.text).join("\n");

const launcher=readFileSync(join(ROOT,"bin","ant-memory.mjs"),"utf8");
check("launcherにMcpServer.prototype.connect monkey patchがない",!launcher.includes("McpServer.prototype.connect"));
check("launcherはnative-server.mjsを起動する",launcher.includes('"native-server.mjs"'));
check("launcherはagent-memory-store/src/index.jsを起動しない",!launcher.includes('"agent-memory-store",\n  "src",\n  "index.js"'));
const where=spawnSync(process.execPath,[join(ROOT,"bin","ant-memory.mjs"),"--where"],{cwd:ROOT,encoding:"utf8",env:{...process.env,ANT_MEMORY_ROOT:ROOT,ANT_MEMORY_WORKSPACE:WS,ANT_MEMORY_CLIENT:"c1-audit"}});
check("--whereのserverEntryがnative server",where.status===0 && /native-server\.mjs/.test(where.stdout),where.stdout+where.stderr);

const c=new Client({name:"phase-c1",version:"1"});
await c.connect(new StdioClientTransport({command:process.execPath,args:[join(ROOT,"bin","ant-memory.mjs")],cwd:ROOT,env:{...process.env,ANT_MEMORY_ROOT:ROOT,ANT_MEMORY_WORKSPACE:WS,ANT_MEMORY_CLIENT:"c1-audit"}}));
try {
  const tools=await c.listTools();
  const names=tools.tools.map(t=>t.name).sort();
  const expected=["change_importance","correct_context","get_context_history","get_state","list_context","list_state","list_unknowns","observe_context","read_context","restore_context","retract_context","search_context","set_state","set_visibility","supersede_context","write_context"].sort();
  check("default surfaceは16 tools",names.length===16,names.join(","));
  check("default surfaceは必要toolだけ",JSON.stringify(names)===JSON.stringify(expected),names.join(","));
  for(const hidden of ["delete_context","promote_memory","search_state","compare_and_set_state","recall_deep","get_successor_context"]){
    check(`defaultで${hidden}を公開しない`,!names.includes(hidden));
  }
  const writeSchema=tools.tools.find(t=>t.name==="write_context")?.inputSchema?.properties??{};
  check("write_contextにagent入力がない",!("agent" in writeSchema),JSON.stringify(writeSchema));

  const w=await c.callTool({name:"write_context",arguments:{topic:"C1 native ownership",content:"NATIVE_SERVER_C1_PROBE",tags:["c1"]}});
  const wt=text(w); const id=wt.match(/id=`([0-9a-f]{10})`/i)?.[1];
  check("native base write成功",!!id,wt);
  check("通常writeにactor補足ノイズがない",!/agent 未指定|正規化/.test(wt),wt);

  const r=await c.callTool({name:"read_context",arguments:{id}});
  check("readがRawとtrusted actorを返す",/NATIVE_SERVER_C1_PROBE/.test(text(r))&&/agent:\*\* c1-audit/.test(text(r)),text(r));
  const s=await c.callTool({name:"search_context",arguments:{query:"NATIVE_SERVER_C1_PROBE",search_mode:"bm25",min_score:0,full_content:true}});
  check("structured searchがnative server上で動く",new RegExp(id).test(text(s)) && /status:\*\* active/.test(text(s)),text(s));

  await c.callTool({name:"set_state",arguments:{key:"c1.state",value:{ok:true}}});
  const st=await c.callTool({name:"get_state",arguments:{key:"c1.state"}});
  check("state primitive互換",/"ok":\s*true/.test(text(st)),text(st));
} finally { await c.close(); }

console.log(`\n結果: ${pass} passed / ${fail} failed`);
process.exit(fail?1:0);
