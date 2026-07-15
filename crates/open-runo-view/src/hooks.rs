//! Phase 2 — hooks相当(state / effect)と再レンダリングランタイム。
//! (HYBRID_NETWORK_ARCHITECTURE.md §0.9.3 Phase 2)
//!
//! Reactの `useState` / `useEffect` を Rust で再現する。Reactは暗黙の
//! グローバルカーソルでフック順序を管理するが、Rustでは安全性のため
//! **明示的な [`Ctx`] ハンドル**をコンポーネントに渡す設計とする
//! (呼び出し順序に依存する点はReactと同じ「フックのルール」として維持)。
//!
//! - [`Ctx::use_state`] — 型付き状態スロット。`Setter` 経由の更新でdirty化
//! - [`Ctx::use_effect`] — deps が変わったレンダリング後にのみ実行
//! - [`Runtime`] — component(&mut Ctx, &Props) -> VNode を保持し、
//!   `rerender()` が 前回ツリーとの [`Patch`] 列を返す(DOMアプライヤは
//!   Phase 3 / open-easyweb 側でこれを消費)

use crate::{diff, Patch, VNode};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

type Slot = Rc<RefCell<Box<dyn Any>>>;

#[derive(Default)]
struct HookStore {
    slots: Vec<Slot>,
    effect_deps: Vec<Option<u64>>,
    dirty: Rc<RefCell<bool>>,
    /// Phase 4: `use_handler` で登録されたクロージャ。スロット位置がそのまま
    /// 安定した `handler_id` になる(同じ呼び出し位置なら毎レンダリングで
    /// 同じID — Reactの `useCallback` 未使用時の再バインドと同様、実体は
    /// 毎回最新のクロージャに差し替わる)。
    handlers: Vec<Rc<dyn Fn()>>,
}

/// レンダリング1回分のフックコンテキスト。
pub struct Ctx<'a> {
    store: &'a mut HookStore,
    cursor: usize,
    effect_cursor: usize,
    handler_cursor: usize,
    /// 今回のレンダリング後に走らせるeffect。
    pending_effects: Vec<Box<dyn FnOnce()>>,
}

/// 状態更新ハンドル(Reactの `setX` 相当)。`'static` かつ `Clone` なので
/// イベントハンドラやeffectへ自由に渡せる。
pub struct Setter<T: 'static> {
    slot: Slot,
    dirty: Rc<RefCell<bool>>,
    _marker: std::marker::PhantomData<T>,
}

impl<T: 'static> Clone for Setter<T> {
    fn clone(&self) -> Self {
        Self {
            slot: self.slot.clone(),
            dirty: self.dirty.clone(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: 'static> Setter<T> {
    pub fn set(&self, value: T) {
        *self.slot.borrow_mut() = Box::new(value);
        *self.dirty.borrow_mut() = true;
    }
    /// 現在値を読んで更新する(Reactの関数型更新 `setX(x => ..)` 相当)。
    pub fn update(&self, f: impl FnOnce(&T) -> T) {
        let next = {
            let b = self.slot.borrow();
            f(b.downcast_ref::<T>().expect("Setter type mismatch"))
        };
        self.set(next);
    }
}

impl<'a> Ctx<'a> {
    /// 状態スロットを確保/取得する。返り値は (現在値のクローン, Setter)。
    /// フックのルール: 毎レンダリングで同じ順序・同じ型で呼ぶこと。
    pub fn use_state<T: Clone + 'static>(&mut self, init: impl FnOnce() -> T) -> (T, Setter<T>) {
        if self.cursor == self.store.slots.len() {
            self.store
                .slots
                .push(Rc::new(RefCell::new(Box::new(init()))));
        }
        let slot = self.store.slots[self.cursor].clone();
        self.cursor += 1;
        let value = slot
            .borrow()
            .downcast_ref::<T>()
            .expect("use_state type mismatch — hooks must be called in the same order with the same types every render")
            .clone();
        (
            value,
            Setter {
                slot,
                dirty: self.store.dirty.clone(),
                _marker: std::marker::PhantomData,
            },
        )
    }

    /// deps のハッシュが前回と異なるとき、レンダリング完了後に `f` を実行する。
    /// `deps = None` は「毎回実行」(Reactのdeps省略と同じ)。
    pub fn use_effect(&mut self, deps: Option<u64>, f: impl FnOnce() + 'static) {
        let idx = self.effect_cursor;
        self.effect_cursor += 1;
        if idx == self.store.effect_deps.len() {
            self.store.effect_deps.push(None);
            self.pending_effects.push(Box::new(f));
            self.store.effect_deps[idx] = deps;
            return;
        }
        let should_run = match (self.store.effect_deps[idx], deps) {
            (_, None) => true,
            (prev, Some(d)) => prev != Some(d),
        };
        self.store.effect_deps[idx] = deps;
        if should_run {
            self.pending_effects.push(Box::new(f));
        }
    }

    /// 宣言的イベントハンドラを登録し、安定した `handler_id` を返す(Phase 4)。
    /// `VElement::on("click", id)` に渡す。呼び出し位置(フック順序)が
    /// IDの安定性を保証する — 毎レンダリング同じ順序で呼ぶこと。
    /// クロージャの実体は毎レンダリング最新のものに差し替わるため、
    /// 直前レンダリングでキャプチャした値(state等)を安全に使える。
    pub fn use_handler(&mut self, f: impl Fn() + 'static) -> u64 {
        let idx = self.handler_cursor;
        self.handler_cursor += 1;
        if idx == self.store.handlers.len() {
            self.store.handlers.push(Rc::new(f));
        } else {
            self.store.handlers[idx] = Rc::new(f);
        }
        idx as u64
    }
}

/// deps ハッシュの補助(`use_effect(Some(deps_hash(&(a, b))), ..)`)。
pub fn deps_hash<T: std::hash::Hash>(t: &T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

/// ステートフル・コンポーネントのランタイム(Reactのルート相当)。
pub struct Runtime<P> {
    component: Box<dyn Fn(&mut Ctx, &P) -> VNode>,
    store: HookStore,
    last: Option<VNode>,
}

impl<P> Runtime<P> {
    pub fn new(component: impl Fn(&mut Ctx, &P) -> VNode + 'static) -> Self {
        Self {
            component: Box::new(component),
            store: HookStore::default(),
            last: None,
        }
    }

    /// Setterによる更新が保留中か。
    pub fn is_dirty(&self) -> bool {
        *self.store.dirty.borrow()
    }

    /// レンダリングを実行し、前回ツリーとの差分パッチを返す。
    /// 初回は `Patch::Replace{path: [], ..}` 1件(=全体マウント)を返す。
    /// レンダリング後、条件を満たしたeffectを実行する。
    pub fn rerender(&mut self, props: &P) -> Vec<Patch> {
        *self.store.dirty.borrow_mut() = false;
        let mut ctx = Ctx {
            store: &mut self.store,
            cursor: 0,
            effect_cursor: 0,
            handler_cursor: 0,
            pending_effects: vec![],
        };
        let new = (self.component)(&mut ctx, props);
        let effects = std::mem::take(&mut ctx.pending_effects);
        let patches = match &self.last {
            Some(old) => diff(old, &new),
            None => vec![Patch::Replace {
                path: vec![],
                node: new.clone(),
            }],
        };
        self.last = Some(new);
        for e in effects {
            e();
        }
        patches
    }

    /// 現在のツリー(SSR/検査用)。
    pub fn tree(&self) -> Option<&VNode> {
        self.last.as_ref()
    }

    /// `handler_id`(`Ctx::use_handler` が返したID)に対応するハンドラを呼ぶ
    /// (Phase 4)。DOM委譲リスナーやテストからのイベント注入から呼ばれる。
    /// ハンドラ実行が `Setter` を呼べば `is_dirty()` が真になるので、
    /// 呼び出し側は続けて `rerender` → `apply` する。
    pub fn dispatch(&self, handler_id: u64) {
        if let Some(h) = self.store.handlers.get(handler_id as usize) {
            let h = h.clone();
            h();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{h, render_html};

    #[test]
    fn button_click_handler_updates_state_via_dispatch() {
        // ボタンに onClick で increment ハンドラを宣言的にバインドし、
        // Runtime::dispatch(id) 経由で「クリックが起きた」ことをシミュレートする。
        let comp = |ctx: &mut Ctx, _p: &()| {
            let (n, set_n) = ctx.use_state(|| 0i32);
            let on_click = ctx.use_handler(move || set_n.update(|v| v + 1));
            h("button")
                .on("click", on_click)
                .child(format!("count={n}"))
                .build()
        };
        let mut rt = Runtime::new(comp);
        rt.rerender(&());
        assert_eq!(render_html(rt.tree().unwrap()), "<button data-orv-click=\"0\">count=0</button>");

        // クリックをシミュレート
        rt.dispatch(0);
        assert!(rt.is_dirty());
        rt.rerender(&());
        assert_eq!(render_html(rt.tree().unwrap()), "<button data-orv-click=\"0\">count=1</button>");

        rt.dispatch(0);
        rt.rerender(&());
        assert_eq!(render_html(rt.tree().unwrap()), "<button data-orv-click=\"0\">count=2</button>");
    }

    #[test]
    fn handler_id_is_stable_across_renders_at_same_call_site() {
        let comp = |ctx: &mut Ctx, _p: &()| {
            let (_n, set_n) = ctx.use_state(|| 0i32);
            let id = ctx.use_handler(move || set_n.set(1));
            h("i").attr("data-id", &id.to_string()).build()
        };
        let mut rt = Runtime::new(comp);
        rt.rerender(&());
        let first = match rt.tree().unwrap() {
            VNode::Element(e) => e.attrs.iter().find(|(k, _)| k == "data-id").unwrap().1.clone(),
            _ => unreachable!(),
        };
        rt.rerender(&());
        let second = match rt.tree().unwrap() {
            VNode::Element(e) => e.attrs.iter().find(|(k, _)| k == "data-id").unwrap().1.clone(),
            _ => unreachable!(),
        };
        assert_eq!(first, second, "handler_id must stay stable across renders");
    }

    #[test]
    fn dispatch_on_unknown_id_is_a_harmless_noop() {
        let comp = |_ctx: &mut Ctx, _p: &()| h("div").build();
        let rt = Runtime::new(comp);
        rt.dispatch(999); // マウント前・未登録IDでもpanicしない
    }

    #[test]
    fn counter_component_rerenders_with_state() {
        let rt_component = |ctx: &mut Ctx, _props: &()| {
            let (n, set_n) = ctx.use_state(|| 0i32);
            // イベントハンドラ相当としてSetterを外へ渡せることを確認するため、
            // テストではeffect経由で1回だけ自動インクリメントする。
            ctx.use_effect(Some(deps_hash(&"mount")), move || set_n.set(n + 1));
            h("span").child(format!("count={n}")).build()
        };
        let mut rt = Runtime::new(rt_component);

        let p1 = rt.rerender(&());
        assert!(matches!(&p1[0], Patch::Replace { .. }), "first render mounts");
        assert_eq!(render_html(rt.tree().unwrap()), "<span>count=0</span>");
        assert!(rt.is_dirty(), "effect called the setter");

        let p2 = rt.rerender(&());
        assert_eq!(
            p2,
            vec![Patch::SetText {
                path: vec![0],
                text: "count=1".into()
            }],
            "second render must be a minimal text patch, not a full replace"
        );
        assert!(!rt.is_dirty(), "mount effect must not re-run (deps unchanged)");
    }

    #[test]
    fn setter_update_reads_current_value_and_multiple_states_keep_order() {
        let comp = |ctx: &mut Ctx, _p: &()| {
            let (a, set_a) = ctx.use_state(|| 10i32);
            let (s, _set_s) = ctx.use_state(|| String::from("hi"));
            ctx.use_effect(None, move || set_a.update(|v| v * 2));
            h("div").child(format!("{a}/{s}")).build()
        };
        let mut rt = Runtime::new(comp);
        rt.rerender(&());
        rt.rerender(&());
        rt.rerender(&());
        // 10 -> 20 -> 40 (effect deps=None なので毎回実行、直近レンダリングで40が反映済み前の値)
        assert_eq!(render_html(rt.tree().unwrap()), "<div>40/hi</div>");
    }

    #[test]
    fn effect_runs_only_when_deps_change() {
        use std::cell::Cell;
        let count = Rc::new(Cell::new(0));
        let c2 = count.clone();
        let comp = move |ctx: &mut Ctx, dep: &u64| {
            let c3 = c2.clone();
            ctx.use_effect(Some(*dep), move || c3.set(c3.get() + 1));
            h("i").build()
        };
        let mut rt = Runtime::new(comp);
        rt.rerender(&1);
        rt.rerender(&1);
        rt.rerender(&2);
        rt.rerender(&2);
        assert_eq!(count.get(), 2, "effect fires only on dep change (1, then 2)");
    }
}
