import Session, {
	install,
	type Request,
	Server,
	type ServerOptions,
	type SessionOptions,
	type TransportOptions,
} from "@moq/web-transport";

const transport: TransportOptions = { idleTimeout: 60_000, keepAliveInterval: 10_000 };
export const options: SessionOptions = { protocols: ["runtime-test"], ...transport };
export const serverOptions: ServerOptions = { idleTimeout: 0 };
export const bind = (cert: Buffer, key: Buffer): Server => Server.bind("[::]:4443", cert, key, serverOptions);
export const connect = (url: string): WebTransport => new Session(url, options);
export const accept = async (server: Server): Promise<WebTransport | null> => {
	const request: Request | null = await server.accept();
	return request ? request.ok() : null;
};
export { install };
