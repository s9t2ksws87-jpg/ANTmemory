"""Hermetic normal-MCP, Viewer and public-2.x migration checks (Python stdlib only)."""
from pathlib import Path
import argparse, hashlib, json, os, queue, re, socket, sqlite3, subprocess, threading, time, urllib.error, urllib.request, uuid

REPO = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--exe', type=Path)
parser.add_argument('--release-root', type=Path, help='extracted ZIP; verifies native root/model discovery without ANT_MEMORY_* settings')
parser.add_argument('--semantic', action='store_true', help='require real local embedding inference')
parser.add_argument('--migration', action='store_true', help='exercise the archived public-2.x implementation; requires Node 24+')
parser.add_argument('--compat', action='store_true', help='check both Node compatibility entry points in --release-root')
args = parser.parse_args()
if args.compat and not args.release_root:
    parser.error('--compat requires --release-root')
target = Path(os.environ.get('CARGO_TARGET_DIR', str(REPO/'target')))
EXE = (args.exe or target/'release'/('antmemory.exe' if os.name=='nt' else 'antmemory')).resolve()
RUN = REPO/'runtime'/('smoke-'+uuid.uuid4().hex[:8])
RUN.mkdir(parents=True)
LAYOUT = args.release_root.resolve() if args.release_root else RUN/'deployment'
if args.release_root and not EXE.is_relative_to(LAYOUT):
    parser.error('--exe must point inside --release-root')
for name in ('runtime','stores'): (LAYOUT/name).mkdir(parents=True,exist_ok=True)
PROJECT = RUN/'project'; PROJECT.mkdir()
(PROJECT/'Cargo.toml').write_text('# workspace marker\n',encoding='utf-8')
(PROJECT/'fixture.md').write_text('Public fixture evidence.\n',encoding='utf-8')
ENV = {k:v for k,v in os.environ.items() if not k.upper().startswith(('ANT_MEMORY','ANTIGRAVITY','ORT_')) and k not in ('CLAUDE_CODE_SESSION_ID','AGENT_STORE_PATH')}
if not args.release_root: ENV['ANT_MEMORY_ROOT']=str(LAYOUT)
FLAGS = subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0
CHECKS, CALLS, CLIENTS = [], [], []

def check(name, ok):
    CHECKS.append({'name':name,'pass':bool(ok)})
    print(('PASS ' if ok else 'FAIL ')+name,flush=True)
    assert ok,name

def estimate(t):
    cjk=sum(0x3000<=ord(c)<=0x30ff or 0x3400<=ord(c)<=0x4dbf or 0x4e00<=ord(c)<=0x9fff or 0xf900<=ord(c)<=0xfaff for c in t)
    return cjk+(len(t.encode('utf-16-le'))//2-cjk+3)//4

def command(argv, env=ENV):
    p=subprocess.run([str(a) for a in argv],cwd=PROJECT,env=env,creationflags=FLAGS,capture_output=True,text=True,encoding='utf-8',timeout=60)
    (RUN/(f'command-{len(CALLS)}.txt')).write_text(p.stdout+p.stderr,encoding='utf-8')
    assert p.returncode==0,p.stdout+p.stderr
    return p.stdout

class MCP:
    def __init__(self, label, argv=None, env=ENV):
        self.label,self.q,self.n=label,queue.Queue(),0
        self.err=(RUN/(label+'.stderr.log')).open('w',encoding='utf-8')
        self.p=subprocess.Popen([str(a) for a in (argv or [EXE,'serve'])],cwd=PROJECT,env=env,creationflags=FLAGS,
                                stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=self.err,text=True,encoding='utf-8')
        CLIENTS.append(self)
        def reader():
            for line in self.p.stdout:
                try:self.q.put(json.loads(line))
                except ValueError:self.q.put({'error':line})
            self.q.put(None)
        threading.Thread(target=reader,daemon=True).start()
        self.init=self.rpc('initialize',{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'release-smoke','version':'1'}})
    def rpc(self, method, params):
        self.n+=1
        self.p.stdin.write(json.dumps({'jsonrpc':'2.0','id':self.n,'method':method,'params':params},ensure_ascii=False)+'\n');self.p.stdin.flush()
        while True:
            r=self.q.get(timeout=45)
            if r is None:raise RuntimeError('MCP EOF: '+self.label)
            assert not r.get('error'),r
            if r.get('id')==self.n:return r['result']
    def tool(self, name, **kw):
        r=self.rpc('tools/call',{'name':name,'arguments':kw})
        text='\n'.join(c.get('text','') for c in r.get('content',[]))
        assert not r.get('isError'),text
        file=f'{len(CALLS):02}-{self.label}-{name}.txt'
        (RUN/file).write_text(text,encoding='utf-8');CALLS.append({'tool':name,'args':kw,'file':file,'estimate':estimate(text)})
        return text
    def close(self):
        if self.p.poll() is not None:return
        self.p.stdin.close()
        try:self.p.wait(timeout=5)
        except subprocess.TimeoutExpired:self.p.terminate();self.p.wait(timeout=5)
        self.err.close()

def mid(t): return re.search(r'id=`([^`]+)`',t)[1]
def ref(t): return re.search(r'\*\*read:\*\* `([^`]+)`',t)[1]
def goal(t): return re.search(r'札を掲げた: (g\d+)',t)[1]

try:
    a=MCP('first')
    check('MCP version is 4.0.0',a.init['serverInfo']['version']=='4.0.0')
    tools=a.rpc('tools/list',{})['tools'];check('19 normal tools',len(tools)==19)
    raw='Release memory: 原文🧠はそのまま。value=1\nUnknown: real-world usefulness is not established by a smoke test.'
    original=mid(a.tool('write_context',content=raw,anchors=['fixture.md']))
    found=a.tool('search_context',query='Release memory quartzmemory',search_mode='bm25')
    check('bounded search reaches saved memory',original in found and estimate(found)<=1000)
    selected=ref(found)
    check('complete Raw',a.tool('read_context',id=selected).endswith(raw))
    check('partial Raw', '部分Raw' in a.tool('read_context',id=selected,start=0,length=30))
    a.close()
    b=MCP('reconnected')
    b.tool('read_context',id=selected)
    check('search reference learns across reconnection',original in b.tool('search_context',query='quartzmemory',search_mode='bm25'))
    b.tool('read_context',id=selected,verdict='not_relevant')
    check('later rejection suppresses recall and past reads',original not in b.tool('search_context',query='quartzmemory',search_mode='bm25'))
    updated_raw=raw.replace('value=1','value=2')
    updated=mid(b.tool('correct_context',id=original,content=updated_raw,reason='fixture correction'))
    check('correction retains original Raw',b.tool('read_context',id=original).endswith(raw))
    check('new Raw readable',b.tool('read_context',id=updated).endswith(updated_raw))
    b.tool('retract_context',id=updated,reason='fixture withdrawal')
    check('retraction retains Raw',b.tool('read_context',id=updated).endswith(updated_raw))
    b.tool('restore_context',id=updated,reason='fixture restoration')
    b.tool('set_state',key='work.smoke',value={'phase':'test','n':1})
    b.tool('set_state',key='work.smoke',expected_value={'n':1,'phase':'test'},value={'phase':'verified'})
    check('state CAS and exact read',json.loads(b.tool('get_state',key='work.smoke'))=={'phase':'verified'})
    unknown=mid(b.tool('write_context',kind='unknown',content='Fixture unresolved issue'))
    check('UNKNOWN listed',unknown in b.tool('list_unknowns'))
    b.tool('write_context',kind='observation',parent_id=unknown,resolves_unknown=True,content='Fixture issue observed and resolved')
    check('resolved UNKNOWN leaves default list',unknown not in b.tool('list_unknowns'))
    previous=goal(b.tool('post_goal',title='Completed other work',parent='none'));b.tool('close_goal',id=previous)
    current=goal(b.tool('post_goal',title='Current work',parent='none'))
    trace=b.tool('show_goals',id=previous,trace=True,token_budget=300)
    check('goal trace budget',estimate(trace)<=300)
    check('viewing other work does not switch focus',current in b.tool('show_goals') and '焦点' in b.tool('show_goals'))
    report=mid(b.tool('write_context',content='Current work report'))
    db=LAYOUT/'stores'/PROJECT.name/'v4.db'
    with sqlite3.connect(db) as c:linked=c.execute('SELECT goal_id FROM goal_links WHERE memory_id=?',[report]).fetchone()
    check('report remains linked to current work',linked==(current,))
    b.tool('close_goal',id=current)
    inventory=b.tool('list_context');check('inventory is bounded and counted',estimate(inventory)<=1200 and '件中' in inventory)
    if args.semantic:
        status=json.loads(command([EXE,'status']));check('embedding runtime loaded',status.get('embedding') is True)
        semantic=b.tool('search_context',query='記録した決定を思い出す',search_mode='semantic',verbose=True,token_budget=2000)
        check('real semantic inference','semantic=ok' in semantic)
    b.close()
    if args.compat:
        for label, path in (('bin-bridge','bin/ant-memory.mjs'),('server-bridge','server/native-server.mjs')):
            bridge=MCP(label,['node',LAYOUT/path])
            check(label+' reports native version',bridge.init['serverInfo']['version']=='4.0.0')
            check(label+' exposes 19 native tools',len(bridge.rpc('tools/list',{})['tools'])==19)
            bridge.close()
    # The Viewer gets a separate OS-selected port and never opens a browser.
    with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    log=(RUN/'viewer.log').open('w',encoding='utf-8')
    viewer=subprocess.Popen([str(EXE),'viewer','--port',str(port),'--no-open'],cwd=PROJECT,env=ENV,creationflags=FLAGS,stdout=log,stderr=log)
    try:
        url=f'http://127.0.0.1:{port}'
        for _ in range(100):
            try:html=urllib.request.urlopen(url,timeout=1).read().decode();break
            except (OSError,urllib.error.URLError):time.sleep(.05)
        else:raise RuntimeError('viewer did not start')
        check('Viewer serves UI','ANTmemory' in html)
        workspaces=json.load(urllib.request.urlopen(url+'/api/workspaces',timeout=5))
        check('Viewer lists workspaces',PROJECT.name in json.dumps(workspaces))
        try:urllib.request.urlopen(urllib.request.Request(url+'/api/workspaces',data=b'{}',method='POST'),timeout=5);denied=False
        except urllib.error.HTTPError as e:denied=e.code==405
        check('Viewer remains read only',denied)
    finally:viewer.terminate();viewer.wait(timeout=5);log.close()
    if args.migration:
        legacy=RUN/'v2-store';legacy.mkdir()
        legacy_env={**ENV,'AGENT_STORE_PATH':str(legacy)}
        command(['node',REPO/'scripts/v2_fixture.mjs'],legacy_env)
        source=legacy/'store.db';before=hashlib.sha256(source.read_bytes()).hexdigest()
        migrated=RUN/'migration'
        command([EXE,'migrate','--rehearse','--from',source,'--out-dir',migrated,'--workspace-root',PROJECT])
        check('public 2.x migration leaves source DB unchanged',hashlib.sha256(source.read_bytes()).hexdigest()==before)
        converted=migrated/'v4.db'
        with sqlite3.connect(converted) as c:
            body=c.execute('SELECT content FROM memories WHERE id=?',['legacykeep']).fetchone()[0]
            withdrawn=c.execute('SELECT status FROM memory_state WHERE id=?',['legacygone']).fetchone()[0]
            state=c.execute('SELECT value_json FROM state WHERE key=?',['work.fixture']).fetchone()[0]
            extra=c.execute('SELECT count(*) FROM legacy_rows WHERE table_name=?',['fixture_annotations']).fetchone()[0]
        check('public 2.x Raw preserved',body=='Public v2 Raw: 原文🧠を保持する。')
        check('public 2.x lifecycle preserved',withdrawn=='retracted')
        check('public 2.x state preserved',json.loads(state)=={'phase':'preserved','count':2})
        check('public 2.x extra tables retained',extra==1)
    (RUN/'result.json').write_text(json.dumps({'checks':CHECKS,'calls':CALLS,'exe_sha256':hashlib.sha256(EXE.read_bytes()).hexdigest()},ensure_ascii=False,indent=2),encoding='utf-8')
    print(f'PASS {len(CHECKS)} checks; evidence: {RUN}',flush=True)
finally:
    for client in CLIENTS:client.close()
    subprocess.run([str(EXE),'drain'],cwd=PROJECT,env=ENV,creationflags=FLAGS,capture_output=True,timeout=30)
