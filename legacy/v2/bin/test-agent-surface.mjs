#!/usr/bin/env node
/** Post redesign cleanup: simplified surface, no duplicate inputs, Antigravity-safe UX. */
import { rmSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT=resolve(dirname(fileURLToPath(import.meta.url)),"..");
const WS="post-redesign-cleanup-test";
const db=join(ROOT,"stores",WS,"store.db");
for(const p of [db,db+"-wal",db+"-shm"]) try{rmSync(p,{force:true})}catch{}
const { Client }=await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","index.js")).href);
const { StdioClientTransport }=await import(pathToFileURL(join(ROOT,"server","node_modules","@modelcontextprotocol","sdk","dist","esm","client","stdio.js")).href);
let pass=0,fail=0; const check=(n,ok,d="")=>{if(ok){pass++;console.log(`PASS ${n}`)}else{fail++;console.error(`FAIL ${n}${d?` — ${d}`:""}`)}};
const text=r=>(r.content??[]).map(x=>x.text).join("\n"); const idOf=s=>s.match(/id=`([0-9a-f]{10})`/i)?.[1];
async function open(name,advanced=false){const c=new Client({name,version:"1"});await c.connect(new StdioClientTransport({command:process.execPath,args:[join(ROOT,"bin","ant-memory.mjs")],cwd:ROOT,env:{...process.env,ANT_MEMORY_ROOT:ROOT,ANT_MEMORY_WORKSPACE:WS,ANT_MEMORY_CLIENT:name,...(advanced?{ANT_MEMORY_ADVANCED_TOOLS:"1"}:{})}}));return c;}
const ag=await open("antigravity"); let adv;
try{
 const listed=await ag.listTools(); const names=listed.tools.map(t=>t.name).sort();
 check("default tool surfaceは16",names.length===16,names.join(","));
 const schemaChars=JSON.stringify(listed.tools).length; check("tool schemaは旧14321文字から25%以上縮小",schemaChars<10741,`chars=${schemaChars}`);
 for(const n of ["delete_context","promote_memory","search_state","compare_and_set_state","get_successor_context","recall_deep"]) check(`${n}はdefault非公開`,!names.includes(n));
 const write=listed.tools.find(t=>t.name==="write_context"); const wp=write?.inputSchema?.properties??{}; check("write_contextにagent入力なし",!("agent" in wp),JSON.stringify(write?.inputSchema)); check("write_contextにsource_refあり","source_ref" in wp,JSON.stringify(write?.inputSchema));
 const search=listed.tools.find(t=>t.name==="search_context"); const sp=search?.inputSchema?.properties??{}; check("search_contextにmax_results重複なし",!("max_results" in sp)); check("default searchにsession_idなし",!("session_id" in sp));

 const w=text(await ag.callTool({name:"write_context",arguments:{topic:"cleanup normal",content:"ANTIGRAVITY_NORMAL_WRITE",tags:["cleanup"]}})); const id=idOf(w); check("Antigravity通常write成功",!!id,w); check("通常writeにactor注記ノイズなし",!/agent 未指定|正規化|actor=/.test(w),w);
 const r=text(await ag.callTool({name:"read_context",arguments:{id}})); check("trusted actorはreadで確認可能",/agent:\*\* antigravity/.test(r),r);
 check("read_contextがcreated時刻を表示",/created:\*\*/.test(r),r);

 const sw=text(await ag.callTool({name:"write_context",arguments:{topic:"cleanup sourced memory",content:"SOURCED_MEMORY_BODY",source_ref:"file://cleanup/source.txt"}})); const sid=idOf(sw); check("通常memoryにsource_ref付きwrite成功",!!sid,sw);
 const sr=text(await ag.callTool({name:"read_context",arguments:{id:sid}})); check("通常memoryのsource_refがreadで再観測取手になる",/source:\*\* file:\/\/cleanup\/source\.txt/.test(sr),sr);
 const ss=text(await ag.callTool({name:"search_context",arguments:{query:"SOURCED_MEMORY_BODY",search_mode:"bm25",min_score:0,full_content:true}})); check("searchがMemory≠Truth契約を返す",/memoryは過去に保存された記録/.test(ss)&&/source:\*\* file:\/\/cleanup\/source\.txt/.test(ss),ss);

 const u=text(await ag.callTool({name:"observe_context",arguments:{kind:"unknown",topic:"cleanup unknown",content:"UNKNOWN_BODY",source_ref:"experiment://cleanup/raw-001"}})); const uid=idOf(u); check("UNKNOWN作成",!!uid,u);
 const ur=text(await ag.callTool({name:"read_context",arguments:{id:uid}})); check("read_contextがsource_refを同時表示",/source:\*\* experiment:\/\/cleanup\/raw-001/.test(ur),ur);
 const us=text(await ag.callTool({name:"search_context",arguments:{query:"UNKNOWN_BODY",search_mode:"bm25",min_score:0}})); check("search_contextがsource_refを同時表示",/source:\*\* experiment:\/\/cleanup\/raw-001/.test(us),us);
 const ul=text(await ag.callTool({name:"list_context",arguments:{tags:["tk:unknown"],limit:20}})); check("list_contextがsource_refを同時表示",/source:experiment:\/\/cleanup\/raw-001/.test(ul),ul);

 await ag.callTool({name:"set_state",arguments:{key:"cleanup.progress",value:{step:1,a:0}}});
 const ls=text(await ag.callTool({name:"list_state",arguments:{query:"progress"}})); check("list_state(query)がsearch_stateを代替",/cleanup\.progress/.test(ls),ls);
 const ok=text(await ag.callTool({name:"set_state",arguments:{key:"cleanup.progress",expected_value:{step:1,a:0},value:{step:2,a:1}}})); check("set_state(expected_value)がCASを代替",/atomically updated/.test(ok),ok);
 const bad=await ag.callTool({name:"set_state",arguments:{key:"cleanup.progress",expected_value:{step:1,a:0},value:{step:3,a:0}}}); check("stale CASを拒否",/CAS_CONFLICT/.test(text(bad)),text(bad));

 // Advanced surface remains available deliberately, but is opt-in.
 adv=await open("audit-advanced",true); const advTools=await adv.listTools(); const an=advTools.tools.map(t=>t.name);
 check("advanced surfaceは旧保守toolを明示時のみ提供",["recall_deep","get_successor_context","get_recall_log"].every(n=>an.includes(n)),an.join(","));
 const stage1=text(await adv.callTool({name:"recall_deep",arguments:{query:"UNKNOWN_BODY",search_mode:"bm25",min_score:0}})); const rid=stage1.match(/id=`([0-9a-f]{10})`/)?.[1]; check("advanced recall Stage1候補取得",!!rid,stage1);
 const stage2=text(await adv.callTool({name:"recall_deep",arguments:{resolve:true,ids:[rid]}})); check("advanced recall Stage2はquery再入力不要",/UNKNOWN_BODY/.test(stage2),stage2);

 const s1=text(await adv.callTool({name:"write_context",arguments:{topic:"successor probe",content:"v1"}})); const v1=idOf(s1);
 const s2=text(await adv.callTool({name:"supersede_context",arguments:{old_id:v1,topic:"successor probe",content:"audit",reason:"probe"}})); const audit=idOf(s2);
 await adv.callTool({name:"retract_context",arguments:{id:audit,reason:"probe retract"}});
 const succ=text(await adv.callTool({name:"get_successor_context",arguments:{id:v1}})); check("retracted chain tipを現行版と誤表示しない",/chain tip:.*status=retracted.*current=no/s.test(succ)&&/effective current: none/.test(succ),succ);

 // create recall log then ensure query/score are rendered.
 await adv.callTool({name:"search_context",arguments:{query:"UNKNOWN_BODY",search_mode:"bm25",min_score:0,full_content:true}});
 const rl=text(await adv.callTool({name:"get_recall_log",arguments:{id:uid,limit:5}})); check("recall logがqueryとscoreを表示",/score:/.test(rl)&&/query:UNKNOWN_BODY/.test(rl),rl);
}finally{try{await ag.close()}catch{} try{await adv?.close()}catch{}}
console.log(`\n結果: ${pass} passed / ${fail} failed`); process.exit(fail?1:0);
