//! 延後一幀釋放持有 GPU 貼圖的物件。
//!
//! # 為什麼需要
//!
//! egui-wgpu 每一幀的順序是：錄製繪製指令 → **立即銷毀**本幀釋放清單裡的貼圖
//! → 送出指令給 GPU。所以「同一幀裡先用某張貼圖畫了東西、後來又把它 drop」
//! 會讓 GPU 收到一個引用已銷毀貼圖的指令，wgpu 的驗證直接 panic：
//!
//! ```text
//! Error in Queue::submit: Validation Error
//! Caused by: Texture with 'egui_texid_Managed(N)' label has been destroyed
//! ```
//!
//! 膠捲條正好踩中這個組合：它是畫面上最後才畫的元件，點擊事件在主圖已經用
//! 舊貼圖畫完之後才處理；點的那張若在快取裡會當場換圖，舊圖的貼圖跟著 drop。
//! Vulkan 後端沒抓這個錯所以在開發機上一直沒發現，DX12（內顯機器）一點就閃退。
//!
//! # 用法
//!
//! 換圖時不直接覆蓋，把舊圖 [`Bin::retire`] 進來；每幀**第一件事**呼叫
//! [`Bin::sweep`]——那時上一幀的指令早已送出，怎麼釋放都安全。
//! 有東西待釋放時記得請求下一幀，否則貼圖會一直佔著 VRAM 直到下次互動。

/// 延後釋放桶。`T` 通常是持有 `TextureHandle` 的東西。
pub struct Bin<T> {
    items: Vec<T>,
}

impl<T> Default for Bin<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<T> Bin<T> {
    /// 收下一個本幀已經畫過、但邏輯上已被換掉的物件。**不會**立刻 drop。
    pub fn retire(&mut self, item: T) {
        self.items.push(item);
    }

    /// 用 `next` 換掉 `slot` 裡的東西；被換出來的（若有）進桶延後釋放。
    /// 這是「所有換圖都必須走這裡」的入口，避免有人直接指派而 drop 掉舊貼圖。
    pub fn replace(&mut self, slot: &mut Option<T>, next: Option<T>) {
        if let Some(old) = std::mem::replace(slot, next) {
            self.retire(old);
        }
    }

    /// 真正釋放。只能在一幀的最開頭呼叫。
    pub fn sweep(&mut self) {
        self.items.clear();
    }

    /// 還有東西等著釋放嗎？是的話呼叫端要請求下一幀。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// 模擬 TextureHandle：drop 時把旗標打開，讓測試看得到「什麼時候被釋放」
    struct Tex(Rc<Cell<bool>>);
    impl Drop for Tex {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    #[test]
    fn replace_does_not_drop_the_old_item_in_the_same_frame() {
        let dropped = Rc::new(Cell::new(false));
        let mut slot = Some(Tex(dropped.clone()));
        let mut bin = Bin::default();

        // 這一幀：畫過舊圖之後才換圖——舊貼圖絕對不能在這裡消失
        bin.replace(&mut slot, Some(Tex(Rc::new(Cell::new(false)))));
        assert!(
            !dropped.get(),
            "舊貼圖在換圖當幀就被 drop 了，GPU 會引用到已銷毀的貼圖"
        );
        assert!(!bin.is_empty(), "應該有東西等著下一幀釋放");

        // 下一幀開頭：現在才安全
        bin.sweep();
        assert!(dropped.get(), "sweep 之後舊貼圖應該已釋放");
        assert!(bin.is_empty());
    }

    #[test]
    fn replacing_with_none_still_defers() {
        // 「清掉目前圖片」跟「換成別張」一樣危險：舊圖本幀可能畫過
        let dropped = Rc::new(Cell::new(false));
        let mut slot = Some(Tex(dropped.clone()));
        let mut bin = Bin::default();
        bin.replace(&mut slot, None);
        assert!(slot.is_none());
        assert!(!dropped.get());
        bin.sweep();
        assert!(dropped.get());
    }

    #[test]
    fn replacing_an_empty_slot_retires_nothing() {
        let mut slot: Option<Tex> = None;
        let mut bin = Bin::default();
        bin.replace(&mut slot, Some(Tex(Rc::new(Cell::new(false)))));
        assert!(bin.is_empty(), "沒有舊東西就不該有待釋放項目");
    }

    #[test]
    fn multiple_replacements_in_one_frame_all_survive_until_sweep() {
        // 快速連點膠捲條：一幀內可能換好幾次
        let flags: Vec<_> = (0..3).map(|_| Rc::new(Cell::new(false))).collect();
        let mut slot = Some(Tex(flags[0].clone()));
        let mut bin = Bin::default();
        bin.replace(&mut slot, Some(Tex(flags[1].clone())));
        bin.replace(&mut slot, Some(Tex(flags[2].clone())));
        assert!(
            flags.iter().all(|f| !f.get()),
            "同一幀內換出的每一張都要活到 sweep"
        );
        bin.sweep();
        assert!(
            flags[0].get() && flags[1].get(),
            "換出的兩張在 sweep 後釋放"
        );
        assert!(!flags[2].get(), "目前這張還在 slot 裡，不該被釋放");
    }
}
