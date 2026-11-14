use ruscello_browser::protocol::{CHALLENGE_SELECTOR, MAX_HTML, MAX_RESOURCES, Step, Url};
use serde::Deserialize;
use serde_json::json;

// The web page receives no bridge to the parent process. The native helper pulls this
// bounded observation buffer through script evaluation after a load or a DOM change.
pub(super) const CAPTURE_RESOURCES: &str = r#"
(() => {
    const urls = new Set();
    const record = raw => {
        try {
            const url = new URL(raw, document.baseURI);
            if (urls.size < 512 && url.href.length <= 4096 && /^https?:$/.test(url.protocol)) {
                urls.add(url.href);
            }
        } catch (_) {}
    };
    const fetch = window.fetch;
    window.fetch = function(input, ...args) {
        record(input instanceof Request ? input.url : input);
        return fetch.call(this, input, ...args);
    };
    const open = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function(method, url, ...args) {
        record(url);
        return open.call(this, method, url, ...args);
    };
    Object.defineProperty(window, '__ruscelloResources', { value: () => {
        for (const entry of performance.getEntriesByType('resource')) record(entry.name);
        for (const node of document.querySelectorAll('video[src], video source[src], audio[src], audio source[src]')) {
            record(node.currentSrc || node.src);
        }
        return Array.from(urls);
    }});
})();
"#;

#[derive(Deserialize)]
pub(super) struct Snapshot {
    pub url: Url,
    #[serde(rename = "ua")]
    pub user_agent: String,
    pub ready: bool,
    pub challenge: bool,
    #[serde(default)]
    pub html: String,
    #[serde(default)]
    pub resources: Vec<String>,
    pub value: Option<String>,
}

pub(super) fn evaluate(step: Option<&Step>, html: bool, resources: bool, expected: &str) -> String {
    let expression = match step {
        Some(Step::Click { selector }) => format!(
            r#"
            const el = document.querySelector({});
            if (el instanceof HTMLElement && !el.matches(':disabled,[aria-disabled=true]') && !forbidden(el)) {{
                el.click(); result.value = 'clicked';
            }}
        "#,
            json!(selector)
        ),
        Some(Step::Submit {
            selector,
            button,
            fields,
        }) => format!(
            r#"
            const form = document.querySelector({selector});
            const buttonSelector = {button};
            const submitter = buttonSelector && form?.querySelector(buttonSelector);
            if (form instanceof HTMLFormElement && !forbidden(form) && !forbidden(submitter) &&
                (!buttonSelector || submitter instanceof HTMLElement && !submitter.matches(':disabled')) &&
                new URL(submitter?.getAttribute('formaction') || form.action, location.href).origin === location.origin) {{
                for (const [name, value] of {fields}) {{
                    const field = Array.from(form.elements).find(el => el.name === name);
                    if (field instanceof HTMLInputElement && field.type !== 'file') {{ field.value = value; }}
                    else if (!field) {{
                        const input = document.createElement('input');
                        input.type = 'hidden'; input.name = name; input.value = value; form.append(input);
                    }}
                }}
                if (submitter) submitter.click(); else HTMLFormElement.prototype.requestSubmit.call(form);
                result.value = 'submitted';
            }}
        "#,
            selector = json!(selector),
            button = json!(button),
            fields = json!(fields)
        ),
        Some(Step::WaitFor { selector }) => format!(
            "if (document.querySelector({})) result.value = 'found';",
            json!(selector)
        ),
        Some(Step::Wait {
            selector: Some(selector),
            ..
        }) => format!(
            r#"
            const el = document.querySelector({});
            if (el) result.value = el.getAttribute('data-seconds') ?? el.getAttribute('data-countdown') ?? el.textContent;
        "#,
            json!(selector)
        ),
        Some(Step::Extract {
            selector: Some(selector),
            attribute,
            ..
        }) => format!(
            r#"
            const el = document.querySelector({});
            const attr = {};
            if (el) result.value = attr ? el.getAttribute(attr) : (el.getAttribute('href') ?? el.getAttribute('src') ?? el.textContent);
        "#,
            json!(selector),
            json!(attribute)
        ),
        _ => String::new(),
    };
    format!(
        r#"
        (() => {{
            const selector = {challenge};
            const result = {{url: location.href, ua: navigator.userAgent,
                ready: document.readyState === 'complete',
                challenge: !!document.querySelector(selector) || /^(just a moment|attention required|verify you are human)/i.test(document.title)}};
            if (!result.ready || result.challenge) return result;
            if ({html}) result.html = document.documentElement?.outerHTML.slice(0, {MAX_HTML}) ?? '';
            if ({resources}) result.resources = (window.__ruscelloResources?.() ?? []).slice(0, {MAX_RESOURCES});
            if (location.origin === {expected}) {{
                const forbidden = el => el && (el.closest(selector) || el.querySelector(selector));
                try {{ {expression} }} catch (_) {{}}
            }}
            if (result.value?.length > 4096) result.value = null;
            return result;
        }})()
    "#,
        challenge = json!(CHALLENGE_SELECTOR),
        expected = json!(expected)
    )
}
