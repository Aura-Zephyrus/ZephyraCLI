use zephyra_core::context::Context;
use zephyra_core::element::Element;
use zephyra_core::id::ElementKey;
use zephyra_core::id::NodeTag;
use zephyra_core::property::PropertyValue;
use zephyra_core::props;
use zephyra_fx::{InputState, View};

const BUTTON_KEY: &str = "sample-button";

/// 输入更新应用状态，view 再把状态声明为下一帧的元素树。
#[derive(Default)]
pub struct App {
    hovered: bool,
    pressed: bool,
    was_pressed: bool,
    clicked: bool,
}

impl View for App {
    fn on_input(&mut self, state: &InputState) {
        let is_hovered = state.hovered.as_ref().and_then(ElementKey::as_name) == Some(BUTTON_KEY);
        let is_pressed = state.pressed.as_ref().and_then(ElementKey::as_name) == Some(BUTTON_KEY);

        if self.was_pressed && !is_pressed {
            self.clicked = !self.clicked;
        }
        self.hovered = is_hovered;
        self.pressed = is_pressed;
        self.was_pressed = is_pressed;
    }

    fn view(&self, _ctx: &Context<'_>) -> Element {
        let label = if self.pressed {
            "Release to activate"
        } else if self.clicked {
            "Clicked! Click again to reset"
        } else if self.hovered {
            "Click me"
        } else {
            "Move pointer here, then click"
        };

        Element::new(NodeTag::Box)
            .with_key("root")
            .with_children([
                Element::new(NodeTag::Text)
                    .with_key(BUTTON_KEY)
                    .with(&props::TEXT, PropertyValue::static_str(label)),
            ])
    }
}
