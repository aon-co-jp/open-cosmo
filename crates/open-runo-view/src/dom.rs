//! Phase 3 — DOMアプライヤ(featureフラグ `dom`、wasm32ブラウザ実行用)。
//! (HYBRID_NETWORK_ARCHITECTURE.md §0.9.3 Phase 3/Phase 4)
//!
//! [`crate::Patch`] 列を実DOMへ適用する。ReactDOMの `render`/commit フェーズに
//! 対応する層で、diff計算(プラットフォーム非依存)と適用(ブラウザ依存)を
//! 分離しているため、コアの `diff`/`Runtime` はnativeでテストしたものが
//! そのままwasmでも動く。
//!
//! **Phase 4(宣言的イベントバインド)**: `VElement::on(event, id)` で宣言された
//! ハンドラは `data-orv-<event>="<id>"` 属性としてDOMに載る(`SetHandler`/
//! `RemoveHandler` パッチ)。実際のリスナは要素ごとに張らず、
//! [`DomMount::attach_with_dispatch`] が**ルートに1つずつ委譲リスナー**を張り、
//! クリック等が起きたら `event.target()` から祖先方向に
//! `data-orv-<event>` を持つ要素を探して呼び出し元コールバックへIDを渡す
//! (Reactの合成イベント・ルートデリゲーションと同じ設計判断)。
//!
//! 利用側(open-easyweb等)の典型形:
//! ```ignore
//! let rt = Rc::new(RefCell::new(Runtime::new(app_component)));
//! let mount = Rc::new(DomMount::attach_with_dispatch("app-root", {
//!     let rt = rt.clone();
//!     move |handler_id, ev| { /* rt.borrow_mut() 経由でハンドラを呼び出し再レンダリング */ }
//! })?);
//! mount.apply(&rt.borrow_mut().rerender(&props))?;
//! ```

use crate::{Patch, VElement, VNode, DELEGATED_EVENTS};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, Event, Node};

/// `data-orv-click` のような委譲用属性名を作る。
fn event_attr_name(event: &str) -> String {
    format!("data-orv-{event}")
}

/// マウントポイント。`root_el` の**唯一の子**として仮想ツリーを管理する。
pub struct DomMount {
    document: Document,
    root_el: Element,
    /// 委譲リスナーの `Closure` を保持し続けるための領域(dropすると失効するため)。
    _listeners: Vec<Closure<dyn FnMut(Event)>>,
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
    /// イベント委譲なしでアタッチする(SSR専用ページ等、静的表示のみの用途)。
    pub fn attach(element_id: &str) -> Result<Self, DomError> {
        let (document, root_el) = resolve_root(element_id)?;
        Ok(Self {
            document,
            root_el,
            _listeners: vec![],
        })
    }

    /// イベント委譲付きでアタッチする。`dispatch` は
    /// `(handler_id, web_sys::Event)` を受け取るコールバックで、通常は
    /// `Runtime` を呼び出して再レンダリング → `apply` するループを組む。
    /// `DELEGATED_EVENTS`(click/input/change/submit)それぞれについて
    /// ルート要素へ1つずつリスナーを張る(要素数に依らず一定コスト)。
    pub fn attach_with_dispatch<F>(element_id: &str, dispatch: F) -> Result<Self, DomError>
    where
        F: Fn(u64, Event) + 'static,
    {
        let (document, root_el) = resolve_root(element_id)?;
        let dispatch = std::rc::Rc::new(dispatch);
        let mut listeners = Vec::with_capacity(DELEGATED_EVENTS.len());
        for &event_name in DELEGATED_EVENTS {
            let attr = event_attr_name(event_name);
            let d = dispatch.clone();
            let closure = Closure::<dyn FnMut(Event)>::new(move |ev: Event| {
                if let Some(target) = ev.target() {
                    if let Ok(el) = target.dyn_into::<Element>() {
                        if let Some(id) = find_handler_id(&el, &attr) {
                            d(id, ev);
                        }
                    }
                }
            });
            root_el.add_event_listener_with_callback(
                event_name,
                closure.as_ref().unchecked_ref(),
            )?;
            listeners.push(closure);
        }
        Ok(Self {
            document,
            root_el,
            _listeners: listeners,
        })
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
            Patch::SetHandler { path, event, handler_id } => {
                let el = self.element_at(path)?;
                el.set_attribute(&event_attr_name(event), &handler_id.to_string())?;
                Ok(())
            }
            Patch::RemoveHandler { path, event } => {
                let el = self.element_at(path)?;
                el.remove_attribute(&event_attr_name(event))?;
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
                events,
                children,
                ..
            }) => {
                let el = self.document.create_element(tag)?;
                for (k, val) in attrs {
                    el.set_attribute(k, val)?;
                }
                for (event, id) in events {
                    el.set_attribute(&event_attr_name(event), &id.to_string())?;
                }
                for c in children {
                    el.append_child(&self.create_node(c)?)?;
                }
                el.into()
            }
        })
    }
}

fn resolve_root(element_id: &str) -> Result<(Document, Element), DomError> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or(DomError::NoWindowOrDocument)?;
    let root_el = document
        .get_element_by_id(element_id)
        .ok_or_else(|| DomError::MountNotFound(element_id.to_string()))?;
    Ok((document, root_el))
}

/// `el` から祖先方向(自身含む)へ `attr` 属性を持つ最初の要素のハンドラIDを探す。
/// ルート(委譲リスナーの張り先)に達したら打ち切る。
fn find_handler_id(el: &Element, attr: &str) -> Option<u64> {
    let mut cur = Some(el.clone());
    while let Some(e) = cur {
        if let Some(v) = e.get_attribute(attr) {
            return v.parse().ok();
        }
        cur = e.parent_element();
    }
    None
}

fn nth_child(parent: &Node, index: usize) -> Option<Node> {
    let mut c = parent.first_child();
    for _ in 0..index {
        c = c.and_then(|n| n.next_sibling());
    }
    c
}
