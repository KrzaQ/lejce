// Gives pi the MCP servers of one run, without touching pi's own settings.
// Lejce writes this file to the program's folder and passes it with
// `pi -e <file>`, together with the servers in an environment variable:
//
//   LEJCE_MCP_SERVERS  {"<name>": {"command": ..., "args": [...],
//                                  "description": ...}, ...}
//
// The tools are "direct": declared to the model like pi's own tools, as
// mcp__<name>__<tool>.

export default function (pi: any) {
	const text = process.env.LEJCE_MCP_SERVERS;
	if (!text) return;
	const servers: Record<string, any> = JSON.parse(text);
	for (const [name, server] of Object.entries(servers)) {
		pi.registerMcpServer(name, {
			command: server.command,
			args: server.args ?? [],
			exposure: "direct",
			description: server.description,
		});
	}
}
