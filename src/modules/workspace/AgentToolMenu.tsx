// M13 拥有:AI 面板的「工具」菜单。收起后是一个按钮,展开后按问答/
// 学习/语言分组;只为真实存在的工具渲染条目(未实现的动作不出现)。
// 键盘:Enter/Space/↓ 打开,↑/↓/Home/End 移动,Esc 关闭并把焦点还给
// 触发按钮,外部 pointerdown 关闭;role=menu/menuitem 供读屏识别。
import { useEffect, useRef, useState } from "react";
import { ChevronDown, Wrench } from "lucide-react";
import type { LucideIcon } from "lucide-react";

export type AgentToolItem = {
  id: string;
  label: string;
  hint?: string;
  icon: LucideIcon;
  onSelect: () => void;
};

export type AgentToolGroup = {
  label: string;
  items: AgentToolItem[];
};

const MENU_ID = "agent-tool-menu";

export function AgentToolMenu({
  groups,
  disabled = false,
}: {
  groups: AgentToolGroup[];
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const items = groups.flatMap((group) => group.items);
  const itemIndex = new Map(items.map((item, index) => [item.id, index]));

  function close(returnFocus = true): void {
    setOpen(false);
    if (returnFocus) triggerRef.current?.focus();
  }

  useEffect(() => {
    if (disabled && open) setOpen(false);
  }, [disabled, open]);

  useEffect(() => {
    if (!open) return;
    itemRefs.current[0]?.focus();
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (target === null) return;
      if (menuRef.current?.contains(target) || triggerRef.current?.contains(target)) return;
      setOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open]);

  function focusItem(index: number): void {
    const count = items.length;
    if (count === 0) return;
    const target = (index + count) % count;
    itemRefs.current[target]?.focus();
  }

  function handleMenuKeyDown(event: React.KeyboardEvent<HTMLDivElement>): void {
    const active = itemRefs.current.findIndex((node) => node === document.activeElement);
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        focusItem(active + 1);
        break;
      case "ArrowUp":
        event.preventDefault();
        focusItem(active <= 0 ? items.length - 1 : active - 1);
        break;
      case "Home":
        event.preventDefault();
        focusItem(0);
        break;
      case "End":
        event.preventDefault();
        focusItem(items.length - 1);
        break;
      case "Escape":
        event.preventDefault();
        close();
        break;
      case "Tab":
        setOpen(false);
        break;
      default:
        break;
    }
  }

  return (
    <div className="agent-tool-menu">
      <button
        ref={triggerRef}
        type="button"
        className="agent-tool-trigger agent-shot-btn"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? MENU_ID : undefined}
        aria-label="工具:解释、出题、总结、翻译等"
        title="工具"
        disabled={disabled}
        onClick={() => setOpen((previous) => !previous)}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" && !open) {
            event.preventDefault();
            setOpen(true);
          }
        }}
      >
        <Wrench className="w-4 h-4" aria-hidden="true" />
        <span>工具</span>
        <ChevronDown className="w-3 h-3 agent-tool-caret" aria-hidden="true" />
      </button>

      {open ? (
        <div
          ref={menuRef}
          id={MENU_ID}
          className="agent-tool-popover"
          role="menu"
          aria-label="快捷工具"
          onKeyDown={handleMenuKeyDown}
        >
          {groups.map((group) => (
            <div
              key={group.label}
              className="agent-tool-group"
              role="group"
              aria-label={group.label}
            >
              <p className="agent-tool-group-label" aria-hidden="true">
                {group.label}
              </p>
              {group.items.map((item) => {
                const Icon = item.icon;
                const index = itemIndex.get(item.id) ?? 0;
                return (
                  <button
                    key={item.id}
                    ref={(node) => {
                      itemRefs.current[index] = node;
                    }}
                    type="button"
                    role="menuitem"
                    tabIndex={-1}
                    className="agent-tool-item"
                    onClick={() => {
                      close();
                      item.onSelect();
                    }}
                  >
                    <Icon className="w-3.5 h-3.5" aria-hidden="true" />
                    <span className="agent-tool-item-label">{item.label}</span>
                    {item.hint ? (
                      <span className="agent-tool-item-hint">{item.hint}</span>
                    ) : null}
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}
