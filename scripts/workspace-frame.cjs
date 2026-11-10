// Navigate the grain's workspace shell and return its application frame.
module.exports = async function openProject(frame, project) {
  while (frame.parentFrame()?.parentFrame()) frame = frame.parentFrame();
  await frame.goto(new URL('/__spki/open/' + encodeURIComponent(project), frame.url()).href);
  const element = await frame.locator('#project-preview').elementHandle();
  const app = await element.contentFrame();
  await app.waitForURL(url => url.href !== 'about:blank');
  await app.locator('body').waitFor();
  return app;
};
