import React from 'react';
import { Meteor } from 'meteor/meteor';
import { useFind, useSubscribe } from 'meteor/react-meteor-data';
import { LinksCollection } from '../api/links';

export const Info = () => {
  const isLoading = useSubscribe('links');
  const links = useFind(() => LinksCollection.find());

  const generateMockLink = () => {
    Meteor.call('links.insertMock', (err) => {
      if (err) {
        alert(err.message);
      }
    });
  };

  if(isLoading()) {
    return <div>Loading...</div>;
  }

  return (
    <div>
      <h2>Learn Meteor!</h2>
      <button onClick={generateMockLink} style={{ padding: '8px 16px', marginBottom: '16px', cursor: 'pointer' }}>
        Generate Mock Link
      </button>
      <ul>{links.map(
        link => <li key={link._id}>
          <a href={link.url} target="_blank">{link.title}</a>
        </li>
      )}</ul>
    </div>
  );
};
