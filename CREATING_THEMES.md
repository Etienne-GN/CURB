# Creating a Custom Theme for CURB

CURB's entire visual style is driven by **13 CSS custom properties** defined on `:root`.
You can override any or all of them by pasting a JSON object into the **Settings → Custom Theme** textarea.

---

## Variable reference

| Variable | Purpose | Default (CURB Dark) |
|---|---|---|
| `--app-bg` | Window / page background | `#0b0e14` |
| `--surface` | Sidebar and card surfaces | `#131722` |
| `--elevated` | Raised elements (top bar, table header, popup) | `#1a2030` |
| `--border` | Dividers and borders | `#232a39` |
| `--text-primary` | Main readable text | `#e6e9ef` |
| `--text-muted` | Secondary / label text | `#8a93a6` |
| `--download` | Download rate, graph fill | `#38bdf8` |
| `--upload` | Upload rate, graph fill | `#fb923c` |
| `--brand` | Accent colour (active nav, OK buttons, badges) | `#34d399` |
| `--danger` | Errors, delete actions | `#fb7185` |
| `--lan` | LAN row background tint | `rgba(45,212,191,0.15)` |
| `--lan-text` | LAN row text / badge colour | `#2dd4bf` |
| `--internet` | Internet row background tint | `rgba(129,140,248,0.15)` |
| `--internet-text` | Internet row text / badge colour | `#818cf8` |

---

## Prompt for AI-assisted theme creation

Copy the prompt below and send it to any AI assistant (ChatGPT, Claude, Gemini …).
Replace the text in `<>` brackets with your own description.

---

**Prompt:**

> I am creating a custom theme for CURB, a Linux network-monitor application with a dark sidebar UI.
> The theme should feel **`<describe mood or inspiration, e.g. "warm amber / retro terminal">`**.
>
> Return a single valid JSON object with exactly these 14 keys and nothing else
> (no markdown fences, no comments):
>
> ```
> --app-bg, --surface, --elevated, --border,
> --text-primary, --text-muted,
> --download, --upload, --brand, --danger,
> --lan, --lan-text, --internet, --internet-text
> ```
>
> Rules:
> - All values must be valid CSS colour strings (hex, rgb(), rgba(), hsl(), etc.).
> - `--lan` and `--internet` should be the same colour as `--lan-text` / `--internet-text`
>   but with **low opacity** (e.g. `rgba(r,g,b,0.15)`) for use as a subtle row background tint.
> - Ensure at least 4.5:1 contrast between `--text-primary` and `--app-bg`.
> - `--download` and `--upload` should be visually distinct from each other and from `--brand`.

---

Once the AI replies, paste the JSON into the **Custom Theme** textarea in CURB's Settings tab and click **Apply Custom Theme**.

---

## Example custom theme JSON

```json
{
  "--app-bg": "#1a0a00",
  "--surface": "#261200",
  "--elevated": "#331a00",
  "--border": "#4a2800",
  "--text-primary": "#ffe0b2",
  "--text-muted": "#a0744a",
  "--download": "#ffd54f",
  "--upload": "#ff8a65",
  "--brand": "#ffca28",
  "--danger": "#ef5350",
  "--lan": "rgba(255,202,40,0.12)",
  "--lan-text": "#ffca28",
  "--internet": "rgba(255,138,101,0.12)",
  "--internet-text": "#ff8a65"
}
```
