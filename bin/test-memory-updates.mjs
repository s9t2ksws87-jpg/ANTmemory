#!/usr/bin/env node
/** Phase C5 UX after cleanup: correction inheritance, explicit visibility, state discovery + CAS on core tools. */
import { rmSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { DatabaseSync } from "node:sqlite";

const ROOT=resolve(dirname(fileURLToPath(import.meta.url)),"..");
const WS="phase-c5-test";
const dbPath=join(ROOT,"stores",WS,"store.db");
for(const p of [dbPath,dbPath+"-wal",dbPath+"-shm"]) try{rmSync(p,{force:true})}catch{}
const { Client }=await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","index.js")).href);
const { StdioClientTransport }=await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","stdio.js")).href);
let pass=0,fail=0; const check=(n,ok,d="")=>{if(ok){pass++;console.log(`PASS ${n}`)}else{fail++;console.error(`FAIL ${n}${d?` — ${d}`:""}`)}};
const text=r=>(r.content??[]).map(x=>x.text).join("\n"); const idOf=s=>s.match(/id=`([0-9a-f]{10})`/i)?.[1];
const c=new Client({name:"phase-c5",version:"1"});
await c.connect(new StdioClientTransport({command:process.execPath,args:[join(ROOT,"bin","ant-memory.mjs")],cwd:ROOT,env:{...process.env,ANT_MEMORY_ROOT:ROOT,ANT_MEMORY_WORKSPACE:WS,ANT_MEMORY_CLIENT:"c5-audit"}}));
try{
 const tools=(await c.listTools()).tools.map(t=>t.name);
 for(const n of ["correct_context","set_visibility","list_state","get_state","set_state"]) check(`core tool ${n} 公開`,tools.includes(n));
 for(const n of ["search_state","compare_and_set_state","promote_memory"]) check(`旧tool ${n} はdefault非公開`,!tools.includes(n));
 const w=text(await c.callTool({name:"write_context",arguments:{topic:"C5 inheritance",content:"VALUE=4317",tags:["keep","meta"],importance:"high",ttl_days:30,source_ref:"file://phase-c5/raw-v1.txt"}})); const old=idOf(w); check("元記録作成",!!old,w);
 let db=new DatabaseSync(dbPath); const before=db.prepare("SELECT tags,importance,expires_at FROM chunks WHERE id=?").get(old); db.close();
 const corr=text(await c.callTool({name:"correct_context",arguments:{id:old,content:"VALUE=4318",reason:"one value correction"}})); const neo=idOf(corr); check("correct_context成功",!!neo,corr);
 db=new DatabaseSync(dbPath,{readOnly:true}); const after=db.prepare("SELECT tags,importance,expires_at FROM chunks WHERE id=?").get(neo); db.close();
 check("correct_contextがtags継承",before.tags===after.tags,`${before.tags} -> ${after.tags}`);
 check("correct_contextがimportance継承",before.importance===after.importance,`${before.importance} -> ${after.importance}`);
 check("correct_contextがTTL絶対期限を継承",before.expires_at===after.expires_at,`${before.expires_at} -> ${after.expires_at}`);
 const oldRead=text(await c.callTool({name:"read_context",arguments:{id:old}})); const newRead=text(await c.callTool({name:"read_context",arguments:{id:neo}}));
 check("旧版readがsuperseded/current=noを表示",/status:\*\* superseded/.test(oldRead)&&/current:\*\* no/.test(oldRead),oldRead);
 check("新版readがactive/current=yesを表示",/status:\*\* active/.test(newRead)&&/current:\*\* yes/.test(newRead),newRead);
 check("correct_contextがsource_refを継承",/source:\*\* file:\/\/phase-c5\/raw-v1\.txt/.test(newRead),newRead);
 const vis=text(await c.callTool({name:"set_visibility",arguments:{id:neo,visibility:"archive",reason:"C5 explicit visibility"}})); check("set_visibility明示API成功",/warm → archive/.test(vis),vis);
 const list=text(await c.callTool({name:"list_context",arguments:{limit:20}})); check("list_contextがstatus/visibility/current表示",new RegExp(`${neo}.*status:active.*visibility:archive.*current:yes`).test(list),list);
 await c.callTool({name:"set_state",arguments:{key:"c5.pipeline",value:{step:1,a:0,b:0}}});
 const ls=text(await c.callTool({name:"list_state",arguments:{query:"pipeline",limit:20}})); check("list_state(query)で未知keyを発見可能",/c5\.pipeline/.test(ls),ls);
 const cas1=text(await c.callTool({name:"set_state",arguments:{key:"c5.pipeline",expected_value:{step:1,a:0,b:0},value:{step:2,a:1,b:0}}})); check("set_state expected_value一致時更新成功",/atomically updated/.test(cas1),cas1);
 const cas2=await c.callTool({name:"set_state",arguments:{key:"c5.pipeline",expected_value:{step:1,a:0,b:0},value:{step:3,a:0,b:1}}}); check("set_state stale更新を拒否",/CAS_CONFLICT/.test(text(cas2)),text(cas2));
 const gs=text(await c.callTool({name:"get_state",arguments:{key:"c5.pipeline"}})); check("CAS競合後も成功側の値を保持",/"step":\s*2/.test(gs)&&/"a":\s*1/.test(gs),gs);
 const sup=text(await c.callTool({name:"supersede_context",arguments:{old_id:neo,topic:"C5 inherited supersede",content:"VALUE=4319",source_ref:"conversation://phase-c5/decision-recall",reason:"verify supersede inheritance"}})); const recall=idOf(sup); check("supersede_context省略metadataでも成功",!!recall,sup);
 db=new DatabaseSync(dbPath,{readOnly:true}); const m2=db.prepare("SELECT tags,importance,expires_at FROM chunks WHERE id=?").get(recall); db.close();
 check("supersede_contextもtags/importance/TTL継承",after.tags===m2.tags&&after.importance===m2.importance&&after.expires_at===m2.expires_at,JSON.stringify({after,m2}));
 const recallRead=text(await c.callTool({name:"read_context",arguments:{id:recall}})); check("supersede_contextが新source_refを保存",/source:\*\* conversation:\/\/phase-c5\/decision-recall/.test(recallRead),recallRead);
}finally{await c.close()}
console.log(`\n結果: ${pass} passed / ${fail} failed`); process.exit(fail?1:0);
