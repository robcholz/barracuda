import type { PortalModule } from "../src/runtime";

export interface Field {
  name: string;
  label: string;
  type?: "text" | "password" | "url" | "number" | "checkbox";
  value?: string | number | boolean;
  options?: readonly string[];
  optional?: boolean;
}

export function element<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  text = "",
) {
  const node = document.createElement(tag);
  node.textContent = text;
  return node;
}

export function panel(root: HTMLElement, title: string, description: string) {
  const container = element("section");
  container.className = "plugin-page";
  container.append(element("h2", title), element("p", description));
  root.replaceChildren(container);
  return container;
}

/** Build-time UI helper, bundled into each contributor; never imports another page. */
export function configuration(options: {
  title: string;
  endpoint: string;
  description: string;
  fields: readonly Field[];
  batch?: boolean;
}): PortalModule["mount"] {
  return (root, { signal }) => {
    if (signal.aborted) return;
    const page = panel(root, options.title, options.description);
    page.append(
      element(
        "p",
        "提交新配置 · 不读取设备现有配置。密钥不保存到浏览器；HTTP 连接为明文，仅在可信配置网络使用。",
      ),
    );
    const form = element("form");
    form.autocomplete = "off";
    const fields = element("fieldset");
    fields.append(element("legend", "连接设置"));
    const controls = options.fields.map((field) => {
      const label = element("label", field.label);
      const control = field.options ? element("select") : element("input");
      control.name = field.name;
      if (field.options) {
        for (const value of field.options) {
          const option = element("option", value);
          option.value = value;
          control.append(option);
        }
      } else {
        const input = control as HTMLInputElement;
        input.type = field.type ?? "text";
        input.autocomplete = field.type === "password" ? "new-password" : "off";
        if (field.type === "checkbox") input.checked = field.value === true;
        if (field.type === "number") {
          input.min = "0";
          input.max = "4294967295";
          input.step = "1";
        }
      }
      if (field.value !== undefined && field.type !== "checkbox")
        control.value = String(field.value);
      control.required = !field.optional && field.type !== "checkbox";
      label.append(control);
      fields.append(label);
      return { field, control };
    });
    const submit = element("button", "应用配置");
    submit.type = "submit";
    const status = element("p");
    status.setAttribute("role", "status");
    status.setAttribute("aria-live", "polite");
    form.append(fields, submit, status);
    page.append(form);
    const lifecycle = new AbortController();
    const cleanup = () => {
      lifecycle.abort();
      for (const { control } of controls) control.value = "";
      page.remove();
      signal.removeEventListener("abort", cleanup);
    };
    signal.addEventListener("abort", cleanup, { once: true });
    for (const { control } of controls) {
      control.addEventListener("input", () => control.setCustomValidity(""), {
        signal: lifecycle.signal,
      });
    }
    form.addEventListener(
      "submit",
      async (event) => {
        event.preventDefault();
        if (submit.disabled || lifecycle.signal.aborted) return;
        const body: Record<string, string | number | boolean> = {};
        for (const { field, control } of controls) {
          control.setCustomValidity("");
          const value = control.value;
          if (field.optional && !value) continue;
          if (field.type === "checkbox")
            body[field.name] = (control as HTMLInputElement).checked;
          else if (field.type === "number") {
            const number = Number(value);
            if (
              !value ||
              !Number.isInteger(number) ||
              number < 0 ||
              number > 4294967295
            )
              control.setCustomValidity("请输入有效的非负整数");
            body[field.name] = number;
          } else {
            if (!field.optional && !value.trim())
              control.setCustomValidity("请填写此字段");
            if (field.type === "url" && !/^https?:\/\/[^\s]+$/.test(value))
              control.setCustomValidity("请输入 HTTP 或 HTTPS 地址");
            body[field.name] = value;
          }
        }
        if (!form.reportValidity()) return;
        submit.disabled = true;
        fields.disabled = true;
        status.textContent = "正在提交…";
        const request = new AbortController();
        const cancel = () => request.abort();
        lifecycle.signal.addEventListener("abort", cancel, { once: true });
        const timeout = setTimeout(cancel, 15000);
        try {
          const response = await fetch(options.endpoint, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(options.batch ? [body] : body),
            signal: request.signal,
            cache: "no-store",
            redirect: "error",
          });
          if (lifecycle.signal.aborted) return;
          status.textContent =
            response.status === 204
              ? "设备已接受配置；这不代表已验证上游服务连接。"
              : response.status === 422
                ? "配置被拒绝，请检查参数。"
                : response.status === 404
                  ? "接口不可用，插件可能已停用。"
                  : `提交失败（HTTP ${response.status}）。`;
          if (response.status === 204)
            for (const { field, control } of controls)
              if (field.type === "password") control.value = "";
        } catch {
          if (!lifecycle.signal.aborted)
            status.textContent =
              "未收到设备确认，请检查连接。配置可能已生效，请勿自动重复提交。";
        } finally {
          clearTimeout(timeout);
          lifecycle.signal.removeEventListener("abort", cancel);
          submit.disabled = false;
          fields.disabled = false;
        }
      },
      { signal: lifecycle.signal },
    );
    return cleanup;
  };
}
