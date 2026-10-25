//! 目標の札のツリー（設計案v2 §4・§8）。全体も差分も、同じ正本（goalsとgoal_*イベント）から作る。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use am_domain::Basis;
use am_store::query::{self, GoalRow};

use crate::ctx::{Held, Level, Session, Workspace};
use crate::render::estimate_tokens;

/// C0・C1で、差分だけに頼らず全体を出し直す条件（§8.2）。
pub const REFRESH_AFTER: Duration = Duration::from_secs(30 * 60);
const REFRESH_CHANGES: usize = 10;

/// 常時見える二つのスローガン。詳細ツリーの表示済み判定とは独立。
pub fn summary(ws: &Workspace, s: &Session) -> am_store::Result<String> {
    let t = Tree::load(ws)?;
    let posted: Vec<_> = t.goals.iter().filter(|g| g.state == "posted").collect();
    if posted.is_empty() {
        return Ok(String::new());
    }
    let overall: Vec<_> = posted.iter().filter(|g| g.kind == "overall").collect();
    let a = overall.first().map(|g| format!("{} {}", g.id, g.title)).unwrap_or_else(|| "未設定".into());
    let work = s.focus.as_deref().and_then(|id| t.get(id)).map(|g| format!("{} {}", g.id, g.title)).unwrap_or_else(|| "未選択".into());
    Ok(format!("全体: {a} / 作業: {work}（掲示{}件・show_goals）", posted.len()))
}

pub struct Tree {
    pub goals: Vec<GoalRow>,
    by_id: HashMap<String, usize>,
    children: HashMap<Option<String>, Vec<usize>>,
}

impl Tree {
    pub fn load(ws: &Workspace) -> am_store::Result<Tree> {
        let goals = query::goals(ws.conn())?;
        let by_id = goals.iter().enumerate().map(|(i, g)| (g.id.clone(), i)).collect();
        let mut children: HashMap<Option<String>, Vec<usize>> = HashMap::new();
        for (i, g) in goals.iter().enumerate() {
            children.entry(g.parent.clone()).or_default().push(i);
        }
        // 全体目標を先に、その後は掲げた順。
        for v in children.values_mut() {
            v.sort_by(|a, b| (goals[*a].kind != "overall", &goals[*a].posted_at).cmp(&(goals[*b].kind != "overall", &goals[*b].posted_at)));
        }
        Ok(Tree { goals, by_id, children })
    }

    pub fn get(&self, id: &str) -> Option<&GoalRow> {
        self.by_id.get(id).map(|i| &self.goals[*i])
    }

    pub fn kids(&self, id: Option<&str>) -> Vec<&GoalRow> {
        self.children.get(&id.map(String::from)).map(|v| v.iter().map(|i| &self.goals[*i]).collect()).unwrap_or_default()
    }

    /// 根からの経路（`g1 徒競走 › g2 こんにゃく`）。
    pub fn path(&self, id: &str) -> String {
        let mut chain = vec![];
        let mut cur = self.get(id);
        let mut guard = 0;
        while let Some(g) = cur {
            chain.push(format!("{} {}", g.id, g.title));
            cur = g.parent.as_deref().and_then(|p| self.get(p));
            guard += 1;
            if guard > 50 {
                break;
            }
        }
        chain.reverse();
        chain.join(" › ")
    }

    pub fn subtree_ids(&self, id: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        let mut stack = vec![id.to_string()];
        while let Some(x) = stack.pop() {
            if out.insert(x.clone()) {
                stack.extend(self.kids(Some(&x)).into_iter().map(|g| g.id.clone()));
            }
        }
        out
    }

    /// 掲示中の札が配下にある（自身を除く）。
    pub fn posted_descendants(&self, id: &str) -> Vec<String> {
        let mut v: Vec<String> = self.subtree_ids(id).into_iter().filter(|x| x != id && self.get(x).is_some_and(|g| g.state == "posted")).collect();
        v.sort();
        v
    }

    /// 一覧に出すか。掲示中・今回変化した・配下に掲示中の札がある（下ろした親の下の作業を隠さない。R3）。
    pub fn visible(&self, g: &GoalRow, changes: &HashMap<String, String>) -> bool {
        g.state == "posted" || changes.contains_key(&g.id) || !self.posted_descendants(&g.id).is_empty()
    }

    /// 根（焦点の札、無ければ最上位）。
    pub fn root_of(&self, id: &str) -> String {
        let mut cur = id.to_string();
        while let Some(p) = self.get(&cur).and_then(|g| g.parent.clone()) {
            cur = p;
        }
        cur
    }
}

pub fn state_mark(g: &GoalRow) -> String {
    match g.state.as_str() {
        "done" => "✓完了".into(),
        "dropped" => "✗不要".into(),
        "merged" => format!("→統合 {}", g.merged_into.clone().unwrap_or_default()),
        _ => String::new(),
    }
}

fn line(t: &Tree, g: &GoalRow, depth: usize, focus: Option<&str>, change: Option<&str>) -> String {
    let kind = if g.kind == "overall" { "（全体）" } else { "" };
    let kids = t.kids(Some(&g.id));
    let all_done = !kids.is_empty() && kids.iter().all(|k| k.state == "done");
    let mut s = format!("{}- {} {}{kind}", "  ".repeat(depth), g.id, g.title);
    let mark = state_mark(g);
    if !mark.is_empty() {
        s.push_str(&format!(" {mark}"));
    }
    if all_done && g.state == "posted" {
        s.push_str(" [子は全て完了]");
    }
    let closed = kids.iter().filter(|k| k.state != "posted").count();
    if closed > 0 && g.state == "posted" {
        s.push_str(&format!(" (下ろした子{closed})"));
    }
    if focus == Some(g.id.as_str()) {
        s.push_str(" ◀焦点");
    }
    if let Some(c) = change {
        s.push_str(&format!(" 〔{c}〕"));
    }
    s
}

pub struct Rendered {
    pub text: String,
    pub complete: bool,
}

/// 掲げている札を深さ優先で出す。予算に入らない枝は、数と開き方を明示する（§8.3）。
pub fn render_full(t: &Tree, root: Option<&str>, focus: Option<&str>, changes: &HashMap<String, String>, budget: usize) -> Rendered {
    let mut lines: Vec<String> = vec![];
    let mut used = 0usize;
    let mut omitted: Vec<(String, usize)> = vec![];
    let tops: Vec<&GoalRow> = match root {
        Some(r) => t.get(r).into_iter().collect(),
        None => t.kids(None).into_iter().filter(|g| t.visible(g, changes)).collect(),
    };
    #[allow(clippy::too_many_arguments)]
    fn walk<'a>(
        t: &'a Tree,
        g: &'a GoalRow,
        depth: usize,
        focus: Option<&str>,
        changes: &HashMap<String, String>,
        budget: usize,
        used: &mut usize,
        lines: &mut Vec<String>,
    ) -> usize {
        let l = line(t, g, depth, focus, changes.get(&g.id).map(|s| s.as_str()));
        let cost = estimate_tokens(&l) + 1;
        if *used + cost > budget {
            return t.subtree_ids(&g.id).len();
        }
        *used += cost;
        lines.push(l);
        let mut skipped = 0;
        for k in t.kids(Some(&g.id)) {
            // 下ろした札は、今回変化したものと、配下に掲示中の札があるものだけ出す（普段は親の行に数で出す）。
            if !t.visible(k, changes) {
                continue;
            }
            skipped += walk(t, k, depth + 1, focus, changes, budget, used, lines);
        }
        skipped
    }
    for g in tops {
        let n = walk(t, g, 0, focus, changes, budget, &mut used, &mut lines);
        if n > 0 {
            omitted.push((g.id.clone(), n));
        }
    }
    let complete = omitted.is_empty();
    if !complete {
        let total: usize = omitted.iter().map(|(_, n)| n).sum();
        let detail = omitted.iter().map(|(id, n)| format!("{id}配下({n})")).collect::<Vec<_>>().join(" ");
        let first = &omitted[0].0;
        lines.push(format!("TREE_INCOMPLETE 残り{total}札: {detail} → show_goals(goal=\"{first}\")"));
    }
    Rendered { text: lines.join("\n"), complete }
}

/// 候補（未確認）の行。正式な札とは分けて出す。
pub fn proposals(ws: &Workspace) -> Vec<String> {
    query::open_proposals(ws.conn())
        .unwrap_or_default()
        .into_iter()
        .map(|p| match p.action.as_str() {
            "close" => format!("- 下ろす候補（未確認）: {} {} ← 引用「{}」", p.id, p.goal_id.unwrap_or_default(), crate::render::short(&p.quote, 60)),
            _ => format!("- 候補（未確認）: {} {} ← 引用「{}」", p.id, p.title.unwrap_or_default(), crate::render::short(&p.quote, 60)),
        })
        .collect()
}

fn change_label(kind: &str, g: Option<&GoalRow>) -> String {
    match (kind, g) {
        ("goal_posted", _) => "＋新規".into(),
        ("goal_edited", _) => "✎変更".into(),
        ("goal_closed", Some(g)) => state_mark(g),
        _ => "変化".into(),
    }
}

/// 札の節と、それを返したときに保持する版。
/// 保持版は、呼んだ側が実際にこの文字列を応答に入れたときだけ確定する（R2）。
pub struct Section {
    pub text: String,
    pub held: Option<Held>,
}

impl Section {
    /// 応答に入れたことを確定する。
    pub fn commit(self, s: &mut Session) -> String {
        s.held = self.held;
        self.text
    }
}

/// 札の節。初めて・焦点が変わった・圧縮の後は全体、その後は差分（C2）か、経路つきの変化（C0・C1）。
/// `quiet`（普段の検索）では、札が無ければ何も出さず、変化が無ければ焦点の1行だけにする。
pub fn section(ws: &Workspace, s: &Session, level: Level, force_full: bool, budget: usize, quiet: bool) -> am_store::Result<Section> {
    let t = Tree::load(ws)?;
    let last = query::last_seq(ws.conn())?;
    let props = proposals(ws);
    let focus = s.focus.clone().filter(|f| t.get(f).is_some());
    let mut head = if quiet { "## 札".to_string() } else { format!("## 札 連携:{}", level.as_str()) };
    if let Some(f) = &focus {
        head.push_str(&format!(" 焦点: {}", t.path(f)));
    }
    if t.goals.is_empty() && props.is_empty() {
        if quiet {
            return Ok(Section { text: String::new(), held: Some(Held { seq: last, full_at: Instant::now(), complete: true, root: None }) });
        }
        return Ok(Section {
            text: format!("{head}\n札なし。全体目標は未設定（推測で埋めない）。今回の依頼で進める。作業目標は post_goal で掲げられる"),
            held: Some(Held { seq: last, full_at: Instant::now(), complete: true, root: None }),
        });
    }
    let root = focus.as_deref().map(|f| t.root_of(f));
    let held = s.held.clone().filter(|h| h.root == root);
    let events = match &held {
        Some(h) => query::goal_events_since(ws.conn(), h.seq)?,
        None => vec![],
    };
    let mut changes: HashMap<String, String> = HashMap::new();
    for (_, kind, subject) in &events {
        changes.insert(subject.clone(), change_label(kind, t.get(subject)));
    }
    let need_full = force_full
        || held.is_none()
        || !held.as_ref().is_some_and(|h| h.complete)
        || (level != Level::C2 && (held.as_ref().is_some_and(|h| h.full_at.elapsed() > REFRESH_AFTER) || changes.len() > REFRESH_CHANGES))
        || (level == Level::C2 && changes.len() > REFRESH_CHANGES * 3);
    let mut out = vec![head];
    let new_held = if need_full {
        let r = render_full(&t, root.as_deref(), focus.as_deref(), &changes, budget);
        out.push(r.text);
        Some(Held { seq: last, full_at: Instant::now(), complete: r.complete, root })
    } else if changes.is_empty() {
        let held = held.map(|h| Held { seq: last, ..h });
        if quiet && props.is_empty() {
            let text = focus.as_deref().map(|f| format!("焦点の札: {}（変化なし。全体は show_goals）", t.path(f))).unwrap_or_default();
            return Ok(Section { text, held });
        }
        out.push("札: 前回の表示から変化なし（全体は show_goals）".to_string());
        held
    } else {
        // 差分。どの行にも親への経路を付け、下ろした札は印で分かるようにする。
        let scope: Option<HashSet<String>> = root.as_deref().map(|r| t.subtree_ids(r));
        let mut lines = vec![format!("札の差分（前回の表示から{}件）:", changes.len())];
        for (id, label) in &changes {
            if scope.as_ref().is_some_and(|sc| !sc.contains(id)) {
                continue;
            }
            lines.push(format!("- {} 〔{label}〕", t.path(id)));
        }
        out.push(lines.join("\n"));
        held.map(|h| Held { seq: last, ..h })
    };
    if !props.is_empty() {
        out.push(props.join("\n"));
    }
    // 普段の検索で、掲げている札が無い（下ろした札だけ）なら見出しも出さない。
    if quiet && out.iter().skip(1).all(|l| l.trim().is_empty()) {
        return Ok(Section { text: String::new(), held: new_held });
    }
    Ok(Section { text: out.join("\n"), held: new_held })
}

/// 根拠の表示（show_goals の詳細）。
pub fn basis_lines(ws: &Workspace, g: &GoalRow) -> Vec<String> {
    let mut out = vec![];
    for b in &g.basis {
        match b {
            Basis::Utterance { utterance_id, quote } => {
                let src = query::utterance(ws.conn(), utterance_id)
                    .ok()
                    .flatten()
                    .map(|(client, session, file, line, _)| format!("{client} 会話{session} {file}:{line}"))
                    .unwrap_or_else(|| "照合記録なし".into());
                out.push(format!("根拠: ユーザー発言（照合済み）「{quote}」 ← {src}"));
            }
            Basis::UnverifiedQuote { quote } => out.push(format!("根拠: 引用（会話ログで照合できなかった）「{quote}」")),
            Basis::Parent { goal_id } => out.push(format!("根拠: 親の札 {goal_id}")),
            Basis::Initiative { note, from } => {
                let f = from.as_deref().map(|x| format!("（元の記憶 {x}）")).unwrap_or_default();
                out.push(format!("根拠: Agentの判断「{note}」{f}"));
            }
        }
    }
    if g.kind != "overall" {
        out.push("札の名前はAgentの言い換え。原文は根拠を辿る".into());
    }
    out
}
