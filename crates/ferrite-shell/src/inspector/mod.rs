//! Inspector core per tab.

pub mod console;
pub mod device;
pub mod elements;
pub mod injection;
pub mod storage;
pub mod waterfall;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InspectorPanel {
    #[default]
    Waterfall,
    Elements,
    Console,
    Storage,
    Device,
    Injection,
}

#[derive(Debug, Default)]
pub struct InspectorState {
    pub open: bool,
    pub active: InspectorPanel,
    pub tab_id: Option<uuid::Uuid>,
    pub waterfall: waterfall::WaterfallState,
    pub elements: elements::ElementsState,
    pub console: console::ConsoleState,
    pub storage: storage::StorageState,
    pub device: device::DeviceState,
}

impl InspectorState {
    pub fn attach(&mut self, tab_id: uuid::Uuid) {
        if self.tab_id != Some(tab_id) {
            self.clear();
        }
        self.open = true;
        self.tab_id = Some(tab_id);
    }

    pub fn detach(&mut self) {
        self.open = false;
        self.clear();
    }

    pub fn clear(&mut self) {
        self.waterfall.clear();
        self.elements.clear();
        self.console.clear();
        self.storage.clear();
    }

    pub fn is_attached_to(&self, tab_id: &uuid::Uuid) -> bool {
        self.open && self.tab_id.as_ref() == Some(tab_id)
    }
}
