//! 撤销/重做：全量快照方案。
//!
//! 图元是小体量矢量数据（每个几十字节），全量快照实现最简单且不会出错；
//! 截图位图本体不参与快照。上限 100 步，超出丢弃最旧的。

use crate::model::Element;

const LIMIT: usize = 100;

#[derive(Default)]
pub struct History {
    undo: Vec<Vec<Element>>,
    redo: Vec<Vec<Element>>,
}

impl History {
    /// 在修改发生前记录当前状态。
    pub fn push(&mut self, current: &[Element]) {
        self.undo.push(current.to_vec());
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, current: &mut Vec<Element>) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(current, prev));
        }
    }

    /// 放弃最近一次 push 之后的修改：回滚且不产生重做项。
    /// 用于取消进行中的手势（退化图形、空文本等）。
    pub fn cancel(&mut self, current: &mut Vec<Element>) {
        if let Some(prev) = self.undo.pop() {
            *current = prev;
        }
    }

    /// 结束一次未发生实际修改的手势：若撤销栈顶与当前状态完全一致
    /// （如「点选图元但没拖动」也 push 过快照），把这条空快照弹出，
    /// 避免 Ctrl+Z 出现一次「看似无反应」。
    pub fn drop_noop(&mut self, current: &[Element]) {
        if self.undo.last().is_some_and(|top| *top == current) {
            self.undo.pop();
        }
    }

    pub fn redo(&mut self, current: &mut Vec<Element>) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(current, next));
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}
