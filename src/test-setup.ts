// vitest 全局初始化：界面语言固定为简体中文（node 中 navigator.language 多为 en-US）
import { setLocale } from "./i18n";

setLocale("zh-CN");
