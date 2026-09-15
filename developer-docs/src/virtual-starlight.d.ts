// Type shims for starlight virtual modules used by our component forks
// (Header.astro / Search.astro). Upstream's own components are type-checked
// inside the package; user forks are not covered by the published types.
declare module 'virtual:starlight/components/*' {
	const Component: import('astro/runtime/server/index.js').AstroComponentFactory;
	export default Component;
}

declare module 'virtual:starlight/user-config' {
	const config: {
		pagefind: unknown;
		components: Record<string, string>;
	};
	export default config;
}

declare module 'virtual:starlight/project-context' {
	const project: { trailingSlash: string; build: unknown };
	export default project;
}

declare module 'virtual:starlight/pagefind-config' {
	export const pagefindUserConfig: Record<string, unknown>;
}
