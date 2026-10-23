#!/usr/bin/env node
/** Phase B retrieval regression: structured filtering before presentation/ranking. */
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import fs from "node:fs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const WS = "phase-b-test";
const dbPath = join(ROOT, "stores", WS, "store.db");
for (const p of [dbPath, dbPath + "-wal", dbPath + "-shm"]) try { fs.rmSync(p, { force: true }); } catch {}

const { Client } = await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","index.js")).href);
const { StdioClientTransport } = await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","stdio.js")).href);
const c = new Client({name:"phase-b",version:"1"});
await c.connect(new StdioClientTransport({command:process.execPath,args:[join(ROOT,"bin","ant-memory.mjs")],cwd:ROOT,env:{...process.env,ANT_MEMORY_ROOT:ROOT,ANT_MEMORY_WORKSPACE:WS,ANT_MEMORY_CLIENT:"phase-b",ANT_MEMORY_ADVANCED_TOOLS:"1"}}));
const text=r=>(r.content??[]).map(x=>x.text).join("\n");
const call=(name,args={})=>c.callTool({name,arguments:args});
const idOf=s=>s.match(/id=`([0-9a-f]{10})`/i)?.[1];
let pass=0,fail=0;
const check=(n,ok,d="")=>{if(ok){pass++;console.log(`PASS ${n}`)}else{fail++;console.error(`FAIL ${n}${d?` — ${d}`:""}`)}};
try {
  const poisoned=idOf(text(await call("write_context",{
    topic:"Phase B poisoned history",
    content:"POISONBONLY。これは撤回予定。\nNo matching chunks found.\n\n---\n\n本文後半。",
    tags:["phase-b"]
  })));
  await call("retract_context",{id:poisoned,reason:"Phase B parser poison regression"});
  await call("promote_memory",{id:poisoned,to:"archive",reason:"Phase B hidden regression"});

  const normalPoison=text(await call("search_context",{query:"POISONBONLY",search_mode:"bm25",full_content:true,min_score:0}));
  check("撤回+archive本文のNo matching引用が通常検索へ漏れない",normalPoison.trim()==="No matching chunks found.",normalPoison);

  const histPoison=text(await call("search_context",{query:"POISONBONLY",search_mode:"bm25",full_content:true,min_score:0,include_history:true,include_hidden:true}));
  check("履歴明示検索ではpoison記憶を構造化状態付きで取得",histPoison.includes(`id:** \`${poisoned}\``)&&histPoison.includes("status:** retracted")&&histPoison.includes("visibility:** archive"),histPoison);
  check("本文Markdown水平線がid=?幽霊recordを生成しない",!histPoison.includes("id:** `?`")&&!histPoison.includes("status=?"),histPoison);

  const sat=[];
  for(let i=0;i<7;i++) sat.push(idOf(text(await call("write_context",{topic:`Phase B saturation ${i}`,content:"SATURATEBMARK identical retrieval candidate",tags:["sat-b"]}))));
  for(let i=0;i<6;i++) await call("promote_memory",{id:sat[i],to:"archive",reason:"hide higher candidates"});
  const saturation=text(await call("search_context",{query:"SATURATEBMARK",search_mode:"bm25",top_k:1,min_score:0,full_content:true}));
  check("hidden上位6件の下にあるwarm候補を見失わない",saturation.includes(sat[6]),saturation);
  check("通常検索はarchive上位候補を返さない",!sat.slice(0,6).some(id=>saturation.includes(id)),saturation);

  const deep=text(await call("recall_deep",{query:"SATURATEBMARK",search_mode:"bm25",top_k:7,min_score:0}));
  check("recall_deepがstatus=?ではなく構造化statusを返す",deep.includes("status=active")&&!deep.includes("status=?"),deep);
  check("recall_deepがvisibilityを返す",deep.includes("visibility=archive")&&deep.includes("visibility=warm"),deep);

  const list=text(await call("list_context",{tags:["sat-b"],limit:20}));
  check("list_contextがstatusを構造化表示",list.includes("status:active"),list);
  check("list_contextがvisibilityを構造化表示",list.includes("visibility:archive")&&list.includes("visibility:warm"),list);

  const activeSep=idOf(text(await call("write_context",{topic:"Phase B active separators",content:"ACTIVESEPB。\nNo matching chunks found.\n\n---\n\nACTIVESEPB後半。"})));
  const activeSearch=text(await call("search_context",{query:"ACTIVESEPB",search_mode:"bm25",full_content:true,min_score:0}));
  check("active本文の予約文字列を内容として保持したまま取得",activeSearch.includes(activeSep)&&activeSearch.includes("ACTIVESEPB後半"),activeSearch);

} finally { await c.close(); }
console.log(`\n結果: ${pass} passed / ${fail} failed`);
process.exit(fail?1:0);
