//! open-runo-view — 「第二のReact」Phase 1
//! (HYBRID_NETWORK_ARCHITECTURE.md §0.9.1/§0.9.3 参照)
//!
//! Reactの中核概念を Rust で一から再実装する。Phase 1 の範囲:
//! - [`VNode`] — 仮想DOMノード(element / text / component)
//! - [`Component`] — props → VNode の純粋関数モデル(Reactの関数コンポーネント相当)
//! - [`diff`] — keyed 差分計算(Reactのreconciliation相当)。パッチ列 [`Patch`] を出力
//! - [`render_html`] — SSR用のHTML文字列レンダラ(エスケープ込み)
//!
//! DOM への適用(wasm32 + web-sys)は Phase 2/3 で、本クレートの `Patch` を
//! 消費するアプライヤとして `open-easyweb` 側に実装する。本クレート自体は
//! `no-DOM` 設計なので native でも wasm32 でもビルド・テストできる。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

/// 仮想DOMノード。
#[derive(Debug, Clone, PartialEq)]
pub enum VNode {
    Text(String),
    Element(VElement),
}

#[derive(Debug, Clone, PartialEq)]
pub struct VElement {
    pub tag: String,
    /// 属性(class, id, href, ...)。イベントはPhase 2でハンドラIDとして拡張。
    pub attrs: Vec<(String, String)>,
    /// 兄弟間の同一性判定に使うkey(Reactの`key`と同義)。
    pub key: Option<String>,
    pub children: Vec<VNode>,
}

/// 要素を作る補助(ReactのcreateElement / JSX相当のビルダ)。
pub fn h(tag: &str) -> VElement {
    VElement {
        tag: tag.to_string(),
        attrs: vec![],
        key: None,
        children: vec![],
    }
}

impl VElement {
    pub fn attr(mut self, k: &str, v: &str) -> Self {
        self.attrs.push((k.to_string(), v.to_string()));
        self
    }
    pub fn key(mut self, k: &str) -> Self {
        self.key = Some(k.to_string());
        self
    }
    pub fn child(mut self, c: impl Into<VNode>) -> Self {
        self.children.push(c.into());
        self
    }
    pub fn children(mut self, cs: impl IntoIterator<Item = VNode>) -> Self {
        self.children.extend(cs);
        self
    }
    pub fn build(self) -> VNode {
        VNode::Element(self)
    }
}

impl From<VElement> for VNode {
    fn from(e: VElement) -> Self {
        VNode::Element(e)
    }
}
impl From<&str> for VNode {
    fn from(s: &str) -> Self {
        VNode::Text(s.to_string())
    }
}
impl From<String> for VNode {
    fn from(s: String) -> Self {
        VNode::Text(s)
    }
}

/// 関数コンポーネント: props(任意型)→ VNode の純粋関数。
/// Reactの `function MyComponent(props) { return <.../> }` に対応する。
pub type Component<P> = Rc<dyn Fn(&P) -> VNode>;

/// コンポーネントを生成する補助。
pub fn component<P, F: Fn(&P) -> VNode + 'static>(f: F) -> Component<P> {
    Rc::new(f)
}

/// ノード位置はルートからの子インデックス列で表す(Phase 1 の単純化)。
pub type Path = Vec<usize>;

/// diff の出力するパッチ。Phase 2 の DOM アプライヤがこれを消費する。
#[derive(Debug, Clone, PartialEq)]
pub enum Patch {
    /// path のノードを丸ごと置換。
    Replace { path: Path, node: VNode },
    /// テキストノードの内容変更。
    SetText { path: Path, text: String },
    /// 属性の追加・変更。
    SetAttr { path: Path, name: String, value: String },
    /// 属性の削除。
    RemoveAttr { path: Path, name: String },
    /// 親 path の末尾に子を追加。
    Append { path: Path, node: VNode },
    /// 親 path の index 番目の子を削除。
    RemoveChild { path: Path, index: usize },
    /// 親 path 内で from → to へ子を移動(keyed reorder)。
    MoveChild { path: Path, from: usize, to: usize },
}

/// 旧ツリーと新ツリーの keyed 差分を計算する(Reactのreconciliation相当)。
pub fn diff(old: &VNode, new: &VNode) -> Vec<Patch> {
    let mut patches = vec![];
    diff_node(old, new, &mut vec![], &mut patches);
    patches
}

fn diff_node(old: &VNode, new: &VNode, path: &mut Path, out: &mut Vec<Patch>) {
    match (old, new) {
        (VNode::Text(a), VNode::Text(b)) => {
            if a != b {
                out.push(Patch::SetText {
                    path: path.clone(),
                    text: b.clone(),
                });
            }
        }
        (VNode::Element(a), VNode::Element(b)) if a.tag == b.tag => {
            diff_attrs(a, b, path, out);
            diff_children(a, b, path, out);
        }
        _ => out.push(Patch::Replace {
            path: path.clone(),
            node: new.clone(),
        }),
    }
}

fn diff_attrs(a: &VElement, b: &VElement, path: &Path, out: &mut Vec<Patch>) {
    let old: HashMap<&str, &str> = a.attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let new: HashMap<&str, &str> = b.attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    for (k, v) in &new {
        if old.get(k) != Some(v) {
            out.push(Patch::SetAttr {
                path: path.clone(),
                name: (*k).to_string(),
                value: (*v).to_string(),
            });
        }
    }
    for k in old.keys() {
        if !new.contains_key(k) {
            out.push(Patch::RemoveAttr {
                path: path.clone(),
                name: (*k).to_string(),
            });
        }
    }
}

fn node_key(n: &VNode) -> Option<&str> {
    match n {
        VNode::Element(e) => e.key.as_deref(),
        VNode::Text(_) => None,
    }
}

fn diff_children(a: &VElement, b: &VElement, path: &mut Path, out: &mut Vec<Patch>) {
    let all_keyed = !b.children.is_empty()
        && b.children.iter().all(|c| node_key(c).is_some())
        && a.children.iter().all(|c| node_key(c).is_some());

    if all_keyed {
        // keyed reconciliation: 現在の並びをシミュレートしながら
        // 移動・削除・追加のパッチを生成する。
        let mut cur: Vec<&VNode> = a.children.iter().collect();
        // 1) 新側に存在しない key を後ろから削除
        let new_keys: Vec<&str> = b.children.iter().map(|c| node_key(c).unwrap()).collect();
        let mut i = cur.len();
        while i > 0 {
            i -= 1;
            let k = node_key(cur[i]).unwrap();
            if !new_keys.contains(&k) {
                out.push(Patch::RemoveChild {
                    path: path.clone(),
                    index: i,
                });
                cur.remove(i);
            }
        }
        // 2) 新側の順に、既存なら必要に応じ移動+再帰diff、無ければ追加
        for (target, nb) in b.children.iter().enumerate() {
            let k = node_key(nb).unwrap();
            match cur.iter().position(|c| node_key(c) == Some(k)) {
                Some(from) => {
                    if from != target {
                        out.push(Patch::MoveChild {
                            path: path.clone(),
                            from,
                            to: target,
                        });
                        let n = cur.remove(from);
                        cur.insert(target, n);
                    }
                    path.push(target);
                    diff_node(cur[target], nb, path, out);
                    path.pop();
                }
                None => {
                    // 末尾Append後にtargetへ移動、を1手で表すためAppend+Move。
                    out.push(Patch::Append {
                        path: path.clone(),
                        node: nb.clone(),
                    });
                    let appended_at = cur.len();
                    cur.push(nb);
                    if appended_at != target {
                        out.push(Patch::MoveChild {
                            path: path.clone(),
                            from: appended_at,
                            to: target,
                        });
                        let n = cur.remove(appended_at);
                        cur.insert(target, n);
                    }
                }
            }
        }
    } else {
        // unkeyed: インデックス位置合わせ(Reactのデフォルト挙動と同じ弱点を持つ。
        // リストにはkeyを付けること — これもReact互換)。
        let common = a.children.len().min(b.children.len());
        for idx in 0..common {
            path.push(idx);
            diff_node(&a.children[idx], &b.children[idx], path, out);
            path.pop();
        }
        for idx in (common..a.children.len()).rev() {
            out.push(Patch::RemoveChild {
                path: path.clone(),
                index: idx,
            });
        }
        for nb in &b.children[common..] {
            out.push(Patch::Append {
                path: path.clone(),
                node: nb.clone(),
            });
        }
    }
}

/// HTMLエスケープ(テキスト用)。
fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// HTMLエスケープ(属性値用)。
fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// SSR: VNode ツリーをHTML文字列に描画する(ReactDOMServer.renderToString 相当)。
pub fn render_html(node: &VNode) -> String {
    let mut s = String::new();
    render_into(node, &mut s);
    s
}

fn render_into(node: &VNode, out: &mut String) {
    match node {
        VNode::Text(t) => escape_text(t, out),
        VNode::Element(e) => {
            let _ = write!(out, "<{}", e.tag);
            for (k, v) in &e.attrs {
                let _ = write!(out, " {k}=\"");
                escape_attr(v, out);
                out.push('"');
            }
            if VOID_ELEMENTS.contains(&e.tag.as_str()) {
                out.push_str(" />");
            } else {
                out.push('>');
                for c in &e.children {
                    render_into(c, out);
                }
                let _ = write!(out, "</{}>", e.tag);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn li(k: &str, txt: &str) -> VNode {
        h("li").key(k).child(txt).build()
    }

    #[test]
    fn component_renders_props_to_vnode() {
        struct Props {
            name: String,
        }
        let hello: Component<Props> = component(|p: &Props| {
            h("h1").attr("class", "title").child(format!("Hello, {}", p.name)).build()
        });
        let v = hello(&Props { name: "世界".into() });
        assert_eq!(render_html(&v), "<h1 class=\"title\">Hello, 世界</h1>");
    }

    #[test]
    fn ssr_escapes_text_and_attrs() {
        let v = h("a").attr("href", "/?a=1&b=\"2\"").child("<b>&").build();
        assert_eq!(
            render_html(&v),
            "<a href=\"/?a=1&amp;b=&quot;2&quot;\">&lt;b&gt;&amp;</a>"
        );
    }

    #[test]
    fn ssr_handles_void_elements() {
        let v = h("div").child(h("br").build()).build();
        assert_eq!(render_html(&v), "<div><br /></div>");
    }

    #[test]
    fn diff_detects_text_and_attr_changes() {
        let old = h("p").attr("class", "a").child("x").build();
        let new = h("p").attr("class", "b").attr("id", "p1").child("y").build();
        let ps = diff(&old, &new);
        assert!(ps.contains(&Patch::SetAttr {
            path: vec![],
            name: "class".into(),
            value: "b".into()
        }));
        assert!(ps.contains(&Patch::SetAttr {
            path: vec![],
            name: "id".into(),
            value: "p1".into()
        }));
        assert!(ps.contains(&Patch::SetText {
            path: vec![0],
            text: "y".into()
        }));
    }

    #[test]
    fn diff_replaces_on_tag_change() {
        let old = h("span").build();
        let new = h("div").build();
        let ps = diff(&old, &new);
        assert!(matches!(&ps[0], Patch::Replace { path, .. } if path.is_empty()));
    }

    /// keyed diff の正しさは「パッチを旧ツリーに適用したら新ツリーになる」ことで検証する。
    fn apply(root: &mut VNode, p: &Patch) {
        fn at<'a>(root: &'a mut VNode, path: &[usize]) -> &'a mut VNode {
            let mut n = root;
            for &i in path {
                n = match n {
                    VNode::Element(e) => &mut e.children[i],
                    _ => panic!("path into text"),
                };
            }
            n
        }
        match p {
            Patch::Replace { path, node } => *at(root, path) = node.clone(),
            Patch::SetText { path, text } => {
                if let VNode::Text(t) = at(root, path) {
                    *t = text.clone();
                }
            }
            Patch::SetAttr { path, name, value } => {
                if let VNode::Element(e) = at(root, path) {
                    if let Some(kv) = e.attrs.iter_mut().find(|(k, _)| k == name) {
                        kv.1 = value.clone();
                    } else {
                        e.attrs.push((name.clone(), value.clone()));
                    }
                }
            }
            Patch::RemoveAttr { path, name } => {
                if let VNode::Element(e) = at(root, path) {
                    e.attrs.retain(|(k, _)| k != name);
                }
            }
            Patch::Append { path, node } => {
                if let VNode::Element(e) = at(root, path) {
                    e.children.push(node.clone());
                }
            }
            Patch::RemoveChild { path, index } => {
                if let VNode::Element(e) = at(root, path) {
                    e.children.remove(*index);
                }
            }
            Patch::MoveChild { path, from, to } => {
                if let VNode::Element(e) = at(root, path) {
                    let n = e.children.remove(*from);
                    e.children.insert(*to, n);
                }
            }
        }
    }

    #[test]
    fn keyed_diff_patches_transform_old_into_new() {
        let old = h("ul")
            .children([li("a", "A"), li("b", "B"), li("c", "C"), li("d", "D")])
            .build();
        // 並べ替え + 削除(b) + 追加(e) + テキスト変更(Cの中身)
        let new = h("ul")
            .children([li("d", "D"), li("c", "C2"), li("e", "E"), li("a", "A")])
            .build();
        let ps = diff(&old, &new);
        let mut cur = old.clone();
        for p in &ps {
            apply(&mut cur, p);
        }
        assert_eq!(cur, new, "applying keyed patches must reproduce the new tree");
        // 丸ごとReplaceで誤魔化していないことも確認
        assert!(ps.iter().all(|p| !matches!(p, Patch::Replace { path, .. } if path.is_empty())));
    }

    #[test]
    fn unkeyed_diff_patches_transform_old_into_new() {
        let old = h("div").children(["a".into(), "b".into(), "c".into()]).build();
        let new = h("div").children(["a".into(), "x".into()]).build();
        let ps = diff(&old, &new);
        let mut cur = old.clone();
        for p in &ps {
            apply(&mut cur, p);
        }
        assert_eq!(cur, new);
    }
}
