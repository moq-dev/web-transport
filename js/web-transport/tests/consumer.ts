import Session, { install, type Request, type Server, type SessionOptions } from "@moq/web-transport";

export const options: SessionOptions = { protocols: ["runtime-test"] };
export const connect = (url: string): WebTransport => new Session(url, options);
export const accept = async (server: Server): Promise<WebTransport | null> => {
	const request: Request | null = await server.accept();
	return request ? request.ok() : null;
};
export { install };
