# nah — not-a-harness 🐎

**هارنس سبک و سریع مدل‌های لوکال — با پروتکل ابزارِ متنی (نه JSON)، پشتیبانی RTL برای محتوای فارسی، و صفر وابستگی.**

`nah` یک ایجنت کدنویس است که به مدل لوکال شما (از طریق Ollama یا هر سرور سازگار با OpenAI مثل llama.cpp / LM Studio) وصل می‌شود، فایل‌ها را می‌خواند و می‌نویسد، و با یک پروتکل **متنی ساده** با مدل ارتباط برقرار می‌کند — چون مدل‌های کوچیک (مثل Qwen2.5-Coder) توی JSON گم می‌شوند.

> **English summary:** `nah` is a lean, dependency-free (std-only Rust) coding-agent harness for local LLMs. Instead of JSON tool-calling, models emit simple fenced `tool` blocks (`READ_FILE`, `WRITE_FILE`, `EDIT_FILE`, `LIST_DIR`) that are parsed by a line-oriented state machine. It keeps prompts inside a fixed token budget (working set of files + rolling summary), gates every file write behind a confirmation diff, and ships an English (Claude-Code style) terminal UI with correct RTL rendering for Persian/Arabic content. A first-run setup wizard configures the provider and default model. Phase 2 adds a Tauri GUI. MIT licensed.

---

## ✨ چرا nah؟

| ویژگی | توضیح |
|---|---|
| 🪶 **سبک‌تر از سبک** | کل پروژه فقط std — باینری ~۵۴۰KB، بدون هیچ crate خارجی. برای سیستم‌های ضعیف (i3-2100) عالیه. |
| 🗣 **پروتکل ابزار متنی** | مدل به‌جای JSON، بلاک‌های ساده می‌نویسه. برای مدل‌های 7B بسیار قابل‌اعتمادتر. |
| 🎯 **مدیریت کانتکست** | با بودجه‌ی ثابت token کار میکنه: آخرین N پیام + فایل‌های باز + خلاصه‌ی چرخشی. با 4K هم میشه کد نوشت. |
| 🔒 **ایمنی فایل** | هر نوشتن/ویرایشی diff برمیگردونه و تأیید میخواد. مسیرهای `..` و absolute بلاک میشن. |
| 🌴 **RTL / فارسی** | رابط کاربری انگلیسی (سبک Claude Code)؛ محتوای فارسی (ورودی شما و خروجی مدل) بدون دستکاری (logical order) به ترمینال میره و درست رسم میشه. |
| ⚙️ **ستاپ اولیه** | بار اول یک ویزارد کوتاه provider و آدرس سرور و مدل پیش‌فرض رو تنظیم میکنه (`nah --setup` یا `/setup`). |
| 🤖 **سازگار با Qwen2.5-Coder** | با Ollama و llama.cpp تست شده؛ پرامپت فشرده و دقیق برای مدل‌های کوچیک. |

## 🚀 شروع سریع

```bash
# 1. مدل رو آماده کن (مثلاً با Ollama)
ollama pull qwen2.5-coder:7b

# 2. بساز (نیاز به Rust — یا از باینری‌های GitHub Actions استفاده کن)
cargo build --release

# 3. اجرا کن — بار اول ویزارد ستاپ بالا میاد:
#    انتخاب provider → آدرس سرور → تست اتصال → لیست مدل‌ها → انتخاب مدل پیش‌فرض
./target/release/nah

# 4. یا مستقیم یک کار بده
nah -p "توی فایل src/main.rs یه تابع greet اضافه کن"
```

هر وقت خواستی تنظیمات رو عوض کنی: `nah --setup` یا داخل برنامه `/setup` (و `/model` برای تعویض سریع مدل).

با `--url` میتونی به هر سرور سازگار با OpenAI وصل بشی:

```bash
nah --provider openai --url http://localhost:8080 --model qwen2.5-coder-14b
```

> **نمایش فارسی/RTL:** رابط برنامه انگلیسی است؛ متن فارسی (تایپ شما و خروجی مدل) به‌صورت logical order به ترمینال میره و ترمینال اون رو درست رسم میکنه. اگر فارسی به‌صورت `?` یا مربع دیده شد، مشکل از **فونت ترمینال** است — توی Windows Terminal یا VS Code یک فونت دارای گلیف فارسی انتخاب کنید (مثل Vazirmatn Mono یا DejaVu Sans Mono).

## 🛠 پروتکل ابزار (بخش جادویی)

مدل به‌جای JSON این بلاک‌ها رو مینویسه و `nah` اجراشون میکنه:

````text
```tool
READ_FILE
path: src/main.rs
```

```tool
WRITE_FILE
path: notes/todo.md
content:
هر متنی اینجا...
END_OF_CONTENT
```

```tool
EDIT_FILE
path: src/main.rs
<<<<<<< SEARCH
let a = 1;
=======
let a = 2;
>>>>>>> REPLACE
```

```tool
LIST_DIR
path: src
```
````

- پارسر **state machine** هست (نه regex): بلاک‌های ناقص یا نامعتبر هرگز اجرا نمیشن و به متن برمیگردن.
- `EDIT_FILE` فقط وقتی کار میکنه که متن SEARCH دقیقاً **یک‌جا** پیدا بشه (مثل Aider).
- نتیجه‌ی هر ابزار به‌صورت متن ساده به مدل برمیگرده.

## ⚙️ تنظیمات

```text
--model <name>       نام مدل            (پیش‌فرض: qwen2.5-coder:7b)
--url <url>          آدرس سرور          (پیش‌فرض: http://localhost:11434)
--provider <kind>    ollama | openai
--root <dir>         ریشه‌ی پروژه
--ctx <tokens>       بودجه‌ی کانتکست    (پیش‌فرض: 4096 → num_ctx)
--history <n>        پیام‌های آخر       (پیش‌فرض: 5)
--steps <n>          حداکثر چرخه مدل↔ابزار در هر نوبت
--yes                تأیید خودکار نوشتن فایل
--no-color           بدون رنگ
```

داخل REPL هم `/help` بزنید — با دستورهای `/models`، `/model`، `/ctx`، `/files`، `/reset` و ...

تنظیمات پایدار: فایل `nah.conf` (کنار باینری یا در cwd) با فرمت `key = value`، یا متغیرهای محیطی `NAH_MODEL` / `NAH_URL` / `NAH_PROVIDER`.

## 🧠 مدیریت کانتکست چطور کار میکنه؟

هر درخواست به مدل از این‌ها ساخته میشه (با بودجه‌ی ثابت token):

1. **پرامپت سیستم** + مستند ابزارها (فشرده)
2. **خلاصه‌ی چرخشی** — وقتی تاریخچه زیاد شد، مدل خودش قدیمی‌ها رو خلاصه میکنه (compaction)
3. **کار اصلی کاربر** — همیشه پین میشه و از کانتکست خارج نمیشه
4. **working set فایل‌ها** — محتوای فایل‌هایی که اخیراً خونده/نوشته شدن (جدیدترها اولویت)
5. **آخرین N پیام** (پیش‌فرض ۵، قابل تنظیم)

تخمین token با `chars/4` — ساده، سریع، بدون وابستگی به tokenizer.

## 🗺 نقشه راه

- [x] **فاز ۱ — ترمینال:** REPL با RTL، پروتکل متنی، مدیریت کانتکست، تأیید diff
- [ ] پشتیبانی HTTPS (فعلاً فقط `http://` — سرورهای لوکال معمولاً HTTP هستن)
- [ ] تاریخچه و ویرایش خط پیشرفته در REPL
- [ ] اجرای دستور شل (با تأیید صریح)
- [ ] **فاز ۲ — GUI با Tauri** (همون core رو صدا میزنه، هیچ بازنویسی لازم نیست)
- [ ] اتصال به ACP برای ویرایشگرها

## 🧪 تست

```bash
cargo test              # 37 تست واحد (پارسر، ابزارها، context، diff، JSON)
python3 scripts/mock_ollama.py 11435 &   # سرور ساختگی برای تست e2e
nah --url http://127.0.0.1:11435 --yes -p "..."
```

CI روی GitHub Actions: `fmt` + `clippy -D warnings` + `test` روی لینوکس/ویندوز/مک + تست e2e با سرور ساختگی. با زدن tag به شکل `v*`، باینری برای هر ۴ پلتفرم ساخته و منتشر میشه.

## 🙏 الهام‌ها

- [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) — معماری «همه‌چیز پلاگین است»، سیاست تأیید، و compaction (طراحی؛ کدش TypeScript هست و اینجا از صفر در Rust نوشته شده)
- [Aider](https://github.com/Aider-AI/aider) — فرمت SEARCH/REPLACE برای ویرایش
- [SWE-agent](https://github.com/SWE-agent/SWE-agent) — اثبات اینکه پروتکل متنی برای مدل‌های کوچیک بهتر از JSON است

## 📄 License

MIT — ببینید [LICENSE](LICENSE).
