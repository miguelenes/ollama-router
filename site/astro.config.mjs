// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: 'https://miguelenes.github.io',
  base: '/ollama-router/',
  integrations: [
    starlight({
      title: 'ollama-router',
      description:
        'Mixed CPU+GPU Ollama-compatible fleet proxy — one URL, many Ollama hosts.',
      logo: { src: './src/assets/mark.svg' },
      favicon: '/favicon.svg',
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/miguelenes/ollama-router',
        },
      ],
      customCss: ['./src/styles/custom.css'],
      head: [
        {
          tag: 'meta',
          attrs: {
            property: 'og:image',
            content: '/ollama-router/og.png',
          },
        },
        {
          tag: 'meta',
          attrs: { name: 'twitter:card', content: 'summary_large_image' },
        },
      ],
      sidebar: [
        {
          label: 'Guides',
          items: [{ autogenerate: { directory: 'guides' } }],
        },
        {
          label: 'Ollama API reference',
          items: [{ autogenerate: { directory: 'reference/ollama' } }],
        },
        {
          label: 'OpenAI API reference',
          items: [{ autogenerate: { directory: 'reference/openai' } }],
        },
        {
          label: 'Admin API',
          items: [{ label: 'OpenAPI reference', link: 'reference/admin' }],
        },
        {
          label: 'FAQ',
          items: [{ autogenerate: { directory: 'faq' } }],
        },
      ],
    }),
  ],
});
