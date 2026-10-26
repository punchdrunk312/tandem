import { call, onDragDrop, onEvent, onSnapshot, type Dropped } from "../bridge";

export { call, onEvent, onSnapshot };

export function onFileDrop(onDrop: (d: Dropped) => void, onHover: (over: boolean) => void) {
  void onDragDrop((e) => {
    if (e.type === "enter" || e.type === "over") onHover(true);
    else if (e.type === "leave") onHover(false);
    else if (e.type === "drop") {
      onHover(false);
      if (e.drop?.count) onDrop(e.drop);
    }
  });
}
