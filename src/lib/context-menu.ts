import { Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { LogicalPosition } from "@tauri-apps/api/dpi";

export type ContextMenuItem =
  | { text: string; action: () => void | Promise<void> }
  | "separator";

// Menu and item resources created by earlier calls. They are closed when the
// next menu is built, not after `popup` resolves: on some platforms `popup`
// resolves before the user selects an item.
let previous: { close(): Promise<void> }[] = [];

export async function showContextMenu(
  e: MouseEvent,
  items: ContextMenuItem[],
): Promise<void> {
  e.preventDefault();
  const stale = previous;
  previous = [];
  for (const resource of stale) {
    resource.close().catch(() => {});
  }
  const menuItems = await Promise.all(
    items.map((item) =>
      item === "separator"
        ? PredefinedMenuItem.new({ item: "Separator" })
        : MenuItem.new({ text: item.text, action: item.action }),
    ),
  );
  const menu = await Menu.new({ items: menuItems });
  previous.push(menu, ...menuItems);
  await menu.popup(new LogicalPosition(e.clientX, e.clientY));
}
