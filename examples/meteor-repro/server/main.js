import { Meteor } from 'meteor/meteor';
import { LinksCollection } from '/imports/api/links';
import { NpmModuleMongodb } from 'meteor/npm-mongo';

const { publish } = Meteor;
Meteor.publish = function publishWithDDPRouter(name, fn) {
  Meteor.methods({
    async [`__subscription__${name}`]() {
      const context = { ...this, ready() {}, unblock() {} };
      const maybeCursorOrCursors = await fn.apply(context, arguments);

      const cursors = Array.isArray(maybeCursorOrCursors)
        ? maybeCursorOrCursors
        : maybeCursorOrCursors
          ? [maybeCursorOrCursors]
          : [];

      const cursorDescriptions = cursors.map((cursor) => {
        const cursorDescription = cursor._cursorDescription;
        if (!cursorDescription) {
          console.error('Expected a cursor, got:', cursor);
          throw new Error('CursorExpectedError');
        }
        return cursorDescription;
      });

      return NpmModuleMongodb.BSON.EJSON.stringify(cursorDescriptions);
    },
  });

  return publish.apply(this, arguments);
};

async function insertLink({ title, url }) {
  await LinksCollection.insertAsync({ title, url, createdAt: new Date() });
}

Meteor.startup(async () => {
  // If the Links collection is empty, add some data.
  if (await LinksCollection.find().countAsync() === 0) {
    await insertLink({
      title: 'Do the Tutorial',
      url: 'https://www.meteor.com/tutorials/react/creating-an-app',
    });

    await insertLink({
      title: 'Follow the Guide',
      url: 'https://guide.meteor.com',
    });

    await insertLink({
      title: 'Read the Docs',
      url: 'https://docs.meteor.com',
    });

    await insertLink({
      title: 'Discussions',
      url: 'https://forums.meteor.com',
    });
  }

  // We publish the entire Links collection to all clients.
  // In order to be fetched in real-time to the clients
  Meteor.publish("links", function () {
    return LinksCollection.find();
  });
});
