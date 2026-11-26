// SSH 交互提示：密码（可保存）/ 私钥口令 / 主机指纹确认 / sudo 密码，按到达顺序逐个弹出（仅主窗口）
import { onLocaleChange, t } from "./i18n";
import type { ClientMsg, PromptMsg } from "./protocol";

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const queue: PromptMsg[] = [];
let current: PromptMsg | undefined;
let dialog: HTMLDialogElement;

/** done：最后一个提示关闭后（聚焦终端） */
export function initPrompt(send: (msg: ClientMsg) => void, done: () => void) {
  dialog = $<HTMLDialogElement>("prompt-dialog");
  dialog.addEventListener("close", () => {
    const p = current!;
    const ok = dialog.returnValue === "ok";
    const input = $<HTMLInputElement>("p-input");
    const value = p.kind === "hostKey" ? "yes" : input.value;
    const save = ok && p.kind === "password" && $<HTMLInputElement>("p-save").checked;
    send({ type: "promptReply", reqId: p.reqId, value: ok ? value : null, save });
    input.value = "";
    current = undefined;
    if (queue.length) show();
    else done();
  });
  onLocaleChange(renderPromptText);
}

export function pushPrompt(m: PromptMsg) {
  queue.push(m);
  if (!current) show();
}

/** daemon 断开：丢弃排队的提示并关闭对话框 */
export function resetPrompts() {
  queue.length = 0;
  if (dialog.open) dialog.close();
}

/** 有提示正在显示或排队（期间主机连接测试不计超时） */
export function promptBusy() {
  return !!dialog?.open || queue.length > 0;
}

/** 显示队首的交互提示 */
function show() {
  const p = (current = queue.shift()!);
  renderPromptText();
  $("p-host").textContent = p.host;
  $("p-message").textContent = p.message;
  $("p-fingerprint").textContent = p.fingerprint ?? "";
  $("p-fingerprint").hidden = p.kind !== "hostKey";
  $("p-input").hidden = p.kind === "hostKey";
  $("p-save-row").hidden = p.kind !== "password";
  $<HTMLInputElement>("p-save").checked = false;
  dialog.returnValue = "";
  dialog.showModal();
  if (p.kind !== "hostKey") $("p-input").focus();
}

/** 交互提示的标题与确定按钮（message 由 daemon 按其语言生成）；切换语言时重绘 */
function renderPromptText() {
  const p = current;
  if (!p) return;
  $("p-title").textContent = t(`prompt.${p.kind}`);
  $("p-ok").textContent = p.kind === "hostKey" ? t("prompt.trust") : t("common.ok");
}
