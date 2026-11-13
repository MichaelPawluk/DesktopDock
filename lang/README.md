# Desktop Dock's languages

Desktop Dock follows Windows' display language. Dock settings > Look > Language can pick another
one. English is written in the program itself; every other language is one file here, built into
`desktop-dock.exe`:

| File | Language |
|---|---|
| `de.toml` | Deutsch |
| `fr.toml` | Français |
| `es.toml` | Español |
| `pt-BR.toml` | Português (Brasil) |
| `ru.toml` | Русский |
| `zh-CN.toml` | 中文(简体) |
| `ja.toml` | 日本語 |

The first versions were machine-written. If something reads oddly, please open an issue or a pull
request with the line and your better wording.

## How a file works

Each line under `[text]` is the English text, then its translation:

```toml
"Lock icons" = "Symbole sperren"
```

- **Blanks** in braces are filled in by the program: `"Moved {name}"` becomes "Moved Notepad".
  Keep every blank, with its name unchanged; the words may go around it in any order.
- **Counts:** a text with `{n}` that depends on a number gives one form per plural form of the
  language, in Unicode's order:
  - German, Spanish, French and Portuguese: one, then other.
  - Russian: one, few, many.
  - Chinese and Japanese: a single form.

  ```toml
  "{n} item" = ["{n} Element", "{n} Elemente"]
  ```
- **A place before "|"** marks an English text used in two places that another language may word
  differently: `"uninstall|Remove"` is the uninstall window's button (uninstalling), while
  `"Remove"` is the Items page's (taking an item off the dock). Translate only what's after the "|".
- **"&"** in a text marks the letter underlined for the keyboard. Keep one, on a letter of your
  word.
- **"…"** after a button's text means it opens a window. Keep it.
- **Windows' own names**, such as This PC, Downloads, the Recycle Bin and Settings, are written
  as Windows writes them in your language.

## The style

Short, plain and friendly, as in the English: "Your dock's file couldn't be read", not
"Configuration parse error". Address the reader the way Windows does in your language: "Sie" in
German, "vous" in French, "você" in Brazilian Portuguese. Dialog buttons have a fixed width, so a
shorter word is better than a longer one when both are right.

`cargo test` checks every file: each text the program shows has a translation, each blank and
plural form is there, and nothing is left from texts the program no longer has.
