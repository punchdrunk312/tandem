import { call, onDragDrop, onSnapshot } from "../bridge";

export { call, onSnapshot };

export function onFileDrop(onPaths: (p: string[]) => void, onHover: (over: boolean) => void) {
  void onDragDrop((e) => {
    if (e.type === "enter" || e.type === "over") onHover(true);
    else if (e.type === "leave") onHover(false);
    else if (e.type === "drop") {
      onHover(false);
      if (e.paths?.length) onPaths(e.paths);
    }
  });
}
