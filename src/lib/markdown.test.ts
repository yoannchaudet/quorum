import { describe, expect, it } from 'vitest';
import { renderMarkdown } from './markdown';

describe('renderMarkdown', () => {
  it('renders Markdown while keeping scripts, raw HTML, and unsafe links inert', () => {
    const html = renderMarkdown(
      '# Heading\n<script>alert(1)</script><img src=x onerror=alert(1)>\n[javascript](javascript:alert(1))\n[good](https://example.com)'
    );

    expect(html).toContain('<h1>Heading</h1>');
    expect(html).not.toMatch(/<script|<img|onerror|href="javascript:/i);
    expect(html).toContain('href="https://example.com"');
  });
});
