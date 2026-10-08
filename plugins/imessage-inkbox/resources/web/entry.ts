import {
  ICON_KEY_ROUND,
  ICON_MAIL,
  KIT_STRINGS,
  button,
  callDevice,
  configuredRow,
  definePage,
  fieldControl,
  h,
  header,
  note,
  page,
  readChannel,
  resultCard,
  row,
  settingsForm,
  toastDeviceError,
  type DeviceResult,
} from "../../../captive-portal/resources/web/ui";

const T = {
  zh: {
    lead: "接入 Inkbox 身份与邮件服务。",
    submit: "保存并替换通道",
    method: "方式",
    methodHint: "新建一个 Inkbox 身份，或使用已有的",
    byEmail: "用邮箱新建",
    byEmailHint: "验证码发到你的邮箱",
    byKey: "已有 API Key",
    byKeyHint: "从 Inkbox 控制台复制",
    email: "邮箱",
    emailHint: "用来认领这个 Inkbox 身份",
    address: "邮箱地址",
    send: "发送验证码",
    code: "验证码",
    codeHint: "输入邮件里的 6 位数字",
    sentTo: "验证码已发到",
    sentInbox: "验证码已发到你的邮箱",
    resend: "重新发送",
    verify: "验证",
    identity: "身份",
    identityHint: "设备用这个邮箱收发邮件",
    card: "Inkbox 身份",
    claimed: "已认领",
    claimedToast: "Inkbox 身份已认领",
    account: "账号",
    accountHint: "Inkbox 控制台中的 API Key 与身份",
    console: "打开 Inkbox 控制台",
    tryChat: "去 Web 聊天试试",
  },
  en: {
    lead: "Connect Inkbox identity and mail.",
    submit: "Save and replace channel",
    method: "Method",
    methodHint: "Create an Inkbox identity, or use one you have",
    byEmail: "Create with email",
    byEmailHint: "A code goes to your inbox",
    byKey: "I have an API key",
    byKeyHint: "Copy it from the Inkbox console",
    email: "Email",
    emailHint: "Used to claim this Inkbox identity",
    address: "Email address",
    send: "Send code",
    code: "Code",
    codeHint: "Enter the 6 digits from the email",
    sentTo: "Code sent to",
    sentInbox: "Code sent to your inbox",
    resend: "Resend",
    verify: "Verify",
    identity: "Identity",
    identityHint: "The device sends and receives mail with this address",
    card: "Inkbox identity",
    claimed: "Claimed",
    claimedToast: "Inkbox identity claimed",
    account: "Account",
    accountHint: "The API key and identity from the Inkbox console",
    console: "Open the Inkbox console",
    tryChat: "Try it in Web chat",
  },
};

const SAMPLE_EMAIL = "you@example.com";
const ENDPOINT = "/api/gateway/inkbox";

/** `GET /api/gateway/inkbox`: `signup` is present when the stored configuration came from signup. */
interface InkboxState {
  signup?: {
    email_address?: string;
    claim_status?: string;
    /** The person's address the code went to. */
    human_email?: string;
  };
}
/** Upstream signups and claims can take a TLS handshake or two on the device. */
const FLOW_TIMEOUT = 30_000;

const show = (node: HTMLElement, visible: boolean) =>
  (node.style.display = visible ? "" : "none");

/**
 * The Inkbox page. 「用邮箱新建」 signs an agent up on the device (`POST /api/gateway/inkbox/signup`,
 * which already stores and registers the channel), then claims it with the emailed code
 * (`/verify`, `/resend`). 「已有 API Key」 posts an existing key to `POST /api/gateway/inkbox`.
 * On mount, `GET /api/gateway/inkbox` resumes a signup at its code or claimed step, or shows a
 * channel configured with a key. A signup error with `"retry": true` offers 「重试」: the device
 * kept the signup, so the same email resumes it without a second email.
 */
export const mount = definePage((context) => {
  const { lang } = context;
  const t = T[lang];
  const s = KIT_STRINGS[lang];

  const current = configuredRow("Inkbox", lang);
  const form = settingsForm(
    {
      endpoint: ENDPOINT,
      submit: t.submit,
      rows: [
        {
          title: t.account,
          hint: t.accountHint,
          link: { label: t.console, href: "https://inkbox.ai" },
          fields: [
            { kind: "secret", name: "api_key", label: "API Key" },
            { kind: "text", name: "identity_id", label: "Identity ID" },
          ],
        },
      ],
      advanced: {
        open: true,
        fields: [
          {
            kind: "url",
            name: "api_base",
            label: "API Base URL",
            value: "https://inkbox.ai",
          },
        ],
      },
      onError: (error) => ({ body: error.message }),
      onSuccess: () => {
        current.show(true);
        void context.refreshStatus();
      },
      success: {
        action: {
          label: t.tryChat,
          run: () => context.navigate("imessage-web"),
        },
      },
    },
    context,
  );
  const accountRow = form.element.querySelector<HTMLElement>(".bc-row")!;

  const method = fieldControl(
    {
      kind: "radio",
      name: "method",
      label: t.method,
      value: "email",
      options: [
        {
          value: "email",
          label: t.byEmail,
          hint: t.byEmailHint,
          icon: ICON_MAIL,
        },
        {
          value: "key",
          label: t.byKey,
          hint: t.byKeyHint,
          icon: ICON_KEY_ROUND,
        },
      ],
    },
    lang,
  );
  const methodRow = row(t.method, t.methodHint, lang, method.element);

  const send = button(t.send, lang, { onClick: () => void signup() });
  const email = fieldControl(
    {
      kind: "text",
      name: "email",
      label: t.address,
      placeholder: SAMPLE_EMAIL,
      action: send,
    },
    lang,
  );
  const emailInput = email.element.querySelector("input")!;
  emailInput.type = "email";
  emailInput.autocomplete = "email";
  const emailRow = row(t.email, t.emailHint, lang, email.element);

  const verify = button(t.verify, lang, { onClick: () => void claim() });
  const code = fieldControl(
    {
      kind: "text",
      name: "code",
      label: t.code,
      placeholder: "000000",
      action: verify,
    },
    lang,
  );
  const codeInput = code.element.querySelector("input")!;
  codeInput.inputMode = "numeric";
  codeInput.maxLength = 6;
  codeInput.autocomplete = "one-time-code";
  codeInput.style.letterSpacing = "0.3em";
  code.element.style.maxWidth = "280px";
  const resendError = h("span", {
    class: "bc-hint bc-hint--error",
    role: "alert",
  });
  const sentTo = h("div", { style: { display: "contents" } });
  const codeRow = row(t.code, t.codeHint, lang, sentTo, code.element);

  const identity = h("div", { style: { display: "contents" } });
  const identityRow = row(t.identity, t.identityHint, lang, identity);

  form.element.prepend(
    current.element,
    methodRow,
    emailRow,
    codeRow,
    identityRow,
  );

  let mode: "email" | "key" = "email";
  let step: "email" | "code" | "claimed" = "email";
  /** The address the code went to, as typed, while a signup waits for its code. */
  let human = "";
  let busy = false;

  const paint = () => {
    const byEmail = mode === "email";
    show(emailRow, byEmail && step !== "claimed");
    show(send, step === "email" || emailInput.value.trim() !== human);
    show(codeRow, byEmail && step === "code");
    show(identityRow, byEmail && step === "claimed");
    show(accountRow, !byEmail);
    show(form.footer, !byEmail);
  };
  const go = (next: typeof step) => {
    step = next;
    paint();
  };
  method.element.addEventListener("change", () => {
    mode = method.control.read() === "key" ? "key" : "email";
    paint();
  });
  emailInput.addEventListener("input", paint);

  /** Runs one device call at a time with the buttons disabled; reports what it can't show inline. */
  async function flow<R>(
    endpoint: string,
    body: unknown,
  ): Promise<DeviceResult<R>> {
    busy = true;
    send.disabled = verify.disabled = true;
    const result = await callDevice<R>(context, endpoint, {
      method: "POST",
      body,
      timeoutMs: FLOW_TIMEOUT,
    });
    busy = false;
    if (!context.signal.aborted) send.disabled = verify.disabled = false;
    return result;
  }

  async function signup() {
    if (busy) return;
    const message = email.control.validate();
    email.control.setError(message);
    if (message) return email.control.focus();
    const address = String(email.control.read());
    const result = await flow<{ email_address?: string }>(
      "/api/gateway/inkbox/signup",
      { email: address },
    );
    if (result.kind === "aborted") return;
    if (result.kind !== "ok") {
      if (result.kind === "error" && result.error.error === "invalid_request")
        email.control.setError(
          result.error.message ?? s.rejected,
          result.error.code,
        );
      toastDeviceError(context, result, () => void signup());
      return;
    }
    human = address;
    awaitCode(result.data?.email_address, address);
    context.toast({ kind: "success", title: `${t.sentTo} ${address}` });
    // the device stored and registered the channel before replying
    void context.refreshStatus();
    code.control.focus();
  }

  /** The identity card the claimed step shows: the agent's own mailbox. */
  const showIdentity = (mailbox: string | undefined) =>
    identity.replaceChildren(
      resultCard(
        { title: t.card, badge: t.claimed, sub: mailbox, initial: "@" },
        lang,
      ),
    );

  /** The code step; `address` is where the code went, unknown when resumed after a reload. */
  function awaitCode(mailbox: string | undefined, address?: string) {
    showIdentity(mailbox);
    const sent = note(address ? t.sentTo : t.sentInbox, lang, {
      mono: address,
      action: { label: t.resend, onClick: () => void resend() },
    });
    sent.append(resendError);
    sentTo.replaceChildren(sent);
    resendError.hidden = true;
    code.control.reset();
    current.show(false);
    go("code");
  }

  /** 409: the device holds no signup (or another flow is running); start over from the email. */
  const conflict = (result: DeviceResult<unknown>) => {
    if (result.kind !== "error" || result.error.status !== 409) return false;
    toastDeviceError(context, result);
    go("email");
    email.control.focus();
    return true;
  };

  async function resend() {
    if (busy) return;
    resendError.hidden = true;
    const result = await flow("/api/gateway/inkbox/resend", {});
    if (result.kind === "aborted" || conflict(result)) return;
    if (result.kind === "ok") {
      context.toast({ kind: "success", title: `${t.sentTo} ${human}` });
      return;
    }
    if (
      result.kind === "error" &&
      result.error.error === "verification_failed"
    ) {
      resendError.textContent = result.error.message ?? s.rejected;
      resendError.hidden = false;
    }
    toastDeviceError(context, result, () => void resend());
  }

  async function claim() {
    if (busy) return;
    const value = codeInput.value.trim();
    const message = !value
      ? s.required(t.code)
      : /^\d{6}$/.test(value)
        ? null
        : t.codeHint;
    code.control.setError(message);
    if (message) return code.control.focus();
    const result = await flow<{ claim_status?: string }>(
      "/api/gateway/inkbox/verify",
      { code: value },
    );
    if (result.kind === "aborted" || conflict(result)) return;
    if (result.kind !== "ok") {
      if (result.kind === "error" && result.error.status !== 502)
        code.control.setError(
          result.error.message ?? s.rejected,
          result.error.code,
        );
      toastDeviceError(context, result, () => void claim());
      return;
    }
    const status = result.data?.claim_status;
    if (status && status !== "agent_claimed") {
      code.control.setError(s.rejected, status);
      return;
    }
    context.toast({
      kind: "success",
      title: t.claimedToast,
      action: {
        label: t.tryChat,
        run: () => context.navigate("imessage-web"),
      },
    });
    go("claimed");
    void context.refreshStatus();
  }

  const onEnter = (action: () => void) => (event: KeyboardEvent) => {
    if (event.key !== "Enter") return;
    event.preventDefault();
    action();
  };
  emailInput.addEventListener(
    "keydown",
    onEnter(() => void signup()),
  );
  codeInput.addEventListener(
    "keydown",
    onEnter(() => void claim()),
  );
  paint();
  void readChannel<InkboxState>(context, ENDPOINT).then((state) => {
    // the person may have started on the page already
    if (!state?.configured || busy || step !== "email") return;
    const signup = state.signup;
    if (!signup) return current.show(true);
    if (signup.claim_status === "agent_claimed") {
      showIdentity(signup.email_address);
      go("claimed");
    } else {
      const address = signup.human_email?.trim() || undefined;
      if (address) {
        human = address;
        emailInput.value = address;
      }
      awaitCode(signup.email_address, address);
    }
  });

  return page(
    header(
      {
        title: "Inkbox",
        lead: t.lead,
        icon: new URL("./icon.png", import.meta.url).href,
        figure: "riffle",
        figureWidth: 280,
      },
      lang,
    ),
    form.element,
  );
});
