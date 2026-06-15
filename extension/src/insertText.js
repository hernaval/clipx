// Inserts `content` at the caret of the currently focused editable element.
// Designed to be passed as `func` to chrome.scripting.executeScript — it is
// serialized via .toString() and runs in page context, so it must NOT reference
// any outer variables, imports, or helpers.
export function insertTextAtCaret(content) {
  const el = document.activeElement;
  if (!el) return false;

  const tag = el.tagName;
  if (tag === 'INPUT' || tag === 'TEXTAREA') {
    const start = el.selectionStart ?? el.value.length;
    const end = el.selectionEnd ?? el.value.length;
    if (typeof el.setRangeText === 'function') {
      el.setRangeText(content, start, end, 'end');
    } else {
      el.value = el.value.slice(0, start) + content + el.value.slice(end);
      const pos = start + content.length;
      el.selectionStart = el.selectionEnd = pos;
    }
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    return true;
  }

  if (el.isContentEditable) {
    // execCommand is deprecated but uniquely preserves the page's native undo stack.
    if (document.execCommand && document.execCommand('insertText', false, content)) {
      return true;
    }
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0) return false;
    const range = sel.getRangeAt(0);
    range.deleteContents();
    const node = document.createTextNode(content);
    range.insertNode(node);
    range.setStartAfter(node);
    range.setEndAfter(node);
    sel.removeAllRanges();
    sel.addRange(range);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    return true;
  }

  return false;
}
