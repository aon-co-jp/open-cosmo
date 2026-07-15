//! Phase 3 — DOMアプライヤ(featureフラグ `dom`、wasm32ブラウザ実行用)。
//! (HYBRID_NETWORK_ARCHITECTURE.md §0.9.3 Phase 3)
//!
//! [`crate::Patch`] 列を実DOMへ適用する。ReactDOMの `render`/commit フェーズに
//! 対応する層で、diff計算(プラットフォーム非依存)と適用(ブラウザ依存)を
//! 分離しているため、コアの `diff`/`Runtime` はnativeでテストしたものが
//! そのままwasmでも動く。
//!
//! 利用側(open-easyweb等)の典型形:
//! ```ignore
//! let mut rt = Runtime::new(app_component);
//! let mount = DomMount::attach("app-root")?;      // <div id="app-root">
//! mount.apply(&rt.rerender(&props))?;             // 初回マウント
//! // イベント→Setter→rt.is_dirty()→ rerender→apply のループ
//! ```
//!
//! Phase 3 の範囲: Patch全種の適用 + 初回マウント。イベントリスナの
//! 宣言的バインド(onClick等のVNode属性化)は Phase 4。現段階では利用側が
//! wasm-bindgen `Closure` で要素にリスナを付け、`Setter` を呼ぶ。

use crate::{Patch, VElement, VNode};
use wasm_bindgen::JsValue;
use web_sys::{Document, Element, Node};

/// マウントポイント。`root_el` の**唯一の子**として仮想ツリーを管理する。
pub struct DomMount {
    document: Document,
    root_el: Element,
}

#[derive(Debug)]
pub enum DomError {
    NoWindowOrDocument,
    MountNotFound(String),
    /// パスが現在のDOM構造と一致しない(diffと適用先の不整合)。
    BadPath,
    Js(JsValue),
}

impl From<JsValue> for DomError {
    fn from(v: JsValue) -> Self {
        DomError::Js(v)
    }
}

impl DomMount {
    /// `element_id` の要素にアタッチする。既存の子は初回 `apply` の
    /// `Replace{path: []}` で置き換えられる。
    pub fn attach(element_id: &str) -> Result<Self, DomError> {
        let document = web_sys::window()
            .and_then(|w| w.document())
            .ok_or(DomError::NoWindowOrDocument)?;
        let root_el = document
            .get_element_by_id(element_id)
            .ok_or_else(|| DomError::MountNotFound(element_id.to_string()))?;
        Ok(Self { document, root_el })
    }

    /// パッチ列を順に適用する。`Runtime::rerender` の戻り値をそのまま渡す。
    pub fn apply(&self, patches: &[Patch]) -> Result<(), DomError> {
        for p in patches {
            self.apply_one(p)?;
        }
        Ok(())
    }

    fn apply_one(&self, patch: &Patch) -> Result<(), DomError> {
        match patch {
            Patch::Replace { path, node } => {
                let created = self.create_node(node)?;
                if path.is_empty() {
                    // ルート全置換: root_el の子を全て消して1子にする。
                    while let Some(c) = self.root_el.first_child() {
                        self.root_el.remove_child(&c)?;
                    }
                    self.root_el.append_child(&created)?;
                } else {
                    let old = self.node_at(path)?;
                    let parent = old.parent_node().ok_or(DomError::BadPath)?;
                    parent.replace_child(&created, &old)?;
                }
                Ok(())
            }
            Patch::SetText { path, text } => {
                let n = self.node_at(path)?;
                n.set_text_content(Some(text));
                Ok(())
            }
            Patch::SetAttr { path, name, value } => {
                let el = self.element_at(path)?;
                el.set_attribute(name, value)?;
                Ok(())
            }
            Patch::RemoveAttr { path, name } => {
                let el = self.element_at(path)?;
                el.remove_attribute(name)?;
                Ok(())
            }
            Patch::Append { path, node } => {
                let parent = self.node_at(path)?;
                let created = self.create_node(node)?;
                parent.append_child(&created)?;
                Ok(())
            }
            Patch::RemoveChild { path, index } => {
                let parent = self.node_at(path)?;
                let child = nth_child(&parent, *index).ok_or(DomError::BadPath)?;
                parent.remove_child(&child)?;
                Ok(())
            }
            Patch::MoveChild { path, from, to } => {
                let parent = self.node_at(path)?;
                let moving = nth_child(&parent, *from).ok_or(DomError::BadPath)?;
                parent.remove_child(&moving)?;
                match nth_child(&parent, *to) {
                    Some(anchor) => {
                        parent.insert_before(&moving, Some(&anchor))?;
                    }
                    None => {
                        parent.append_child(&moving)?;
                    }
                }
                Ok(())
            }
        }
    }

    /// 仮想パス(ルートからの子インデックス列)を実DOMノードに解決する。
    /// ルート(path=[])は「root_el の第1子」= 仮想ツリーのルートに対応する。
    fn node_at(&self, path: &[usize]) -> Result<Node, DomError> {
        let mut n: Node = self.root_el.first_child().ok_or(DomError::BadPath)?;
        for &i in path {
            n = nth_child(&n, i).ok_or(DomError::BadPath)?;
        }
        Ok(n)
    }

    fn element_at(&self, path: &[usize]) -> Result<Element, DomError> {
        use wasm_bindgen::JsCast;
        self.node_at(path)?
            .dyn_into::<Element>()
            .map_err(|_| DomError::BadPath)
    }

    /// VNode → 実DOMノードの再帰生成(初回マウント/Replace/Append用)。
    fn create_node(&self, v: &VNode) -> Result<Node, DomError> {
        Ok(match v {
            VNode::Text(t) => self.document.create_text_node(t).into(),
            VNode::Element(VElement {
                tag,
                attrs,
                children,
                ..
            }) => {
                let el = self.document.create_element(tag)?;
                for (k, val) in attrs {
                    el.set_attribute(k, val)?;
                }
                for c in children {
                    el.append_child(&self.create_node(c)?)?;
                }
                el.into()
            }
        })
    }
}

fn nth_child(parent: &Node, index: usize) -> Option<Node> {
    let mut c = parent.first_child();
    for _ in 0..index {
        c = c.and_then(|n| n.next_sibling());
    }
    c
}
