String leftAlign(String text, int width, {String fillChar = ' '}) {
  if (text.length >= width) return text;
  return text + fillChar * (width - text.length);
}
