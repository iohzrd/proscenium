import { Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { LogicalPosition } from "@tauri-apps/api/dpi";

export type ContextMenuItem =
  | { text: string; action: () => void | Promise<void> }
  | "separator";

export async function showContextMenu(
  e: MouseEvent,
  items: ContextMenuItem[],
): Promise<void> {
  e.preventDefault();
  const menuItems = await Promise.all(
    items.map((item) =>
      item === "separator"
        ? PredefinedMenuItem.new({ item: "Separator" })
        : MenuItem.new({ text: item.text, action: item.action }),
    ),
  );
  const menu = await Menu.new({ items: menuItems });
  await menu.popup(new LogicalPosition(e.clientX, e.clientY));
}
