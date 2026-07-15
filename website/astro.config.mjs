// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

const section = (label, directory) => ({
  label,
  collapsed: true,
  autogenerate: { directory },
});

export default defineConfig({
  site: 'https://punarduttrajput.github.io',
  integrations: [
    starlight({
      title: 'Apex',
      description:
        'Apex — Generative UI Trust Runtime, built on an enterprise AI Agent Operating System written in Rust.',
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/punarduttrajput/Apex',
        },
      ],
      sidebar: [
        section('00 · Executive', '00-executive'),
        section('01 · Product', '01-product'),
        section('02 · Architecture', '02-architecture'),
        section('03 · Workflow Engine', '03-workflow-engine'),
        section('04 · Agent Framework', '04-agent-framework'),
        section('05 · LLM Gateway', '05-llm-gateway'),
        section('06 · Memory Engine', '06-memory-engine'),
        section('07 · Tool Runtime', '07-tool-runtime'),
        section('08 · Plugin SDK', '08-plugin-sdk'),
        section('09 · API', '09-api'),
        section('10 · Dashboard', '10-dashboard'),
        section('11 · CLI', '11-cli'),
        section('12 · Deployment', '12-deployment'),
        section('13 · Security', '13-security'),
        section('14 · Observability', '14-observability'),
        section('15 · Testing', '15-testing'),
        section('16 · Examples', '16-examples'),
        section('17 · ADRs', '17-adr'),
        section('18 · Roadmap', '18-roadmap'),
        section('19 · Implementation Guide', '19-implementation-guide'),
      ],
    }),
  ],
});
